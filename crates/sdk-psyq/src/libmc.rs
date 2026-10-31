//! libcard's BIOS wrappers, and libmcx (chip memcard, D59, D65).
//!
//! **libcard**'s wrappers (`InitCARD`, `StartCARD`, `StopCARD`) run as
//! the original code into the BIOS's card functions (`psx-bios::card`,
//! D43); the functions they call to patch Sony BIOS code in kernel RAM are
//! adapters with nothing to patch. libmcrd runs as guest code over the
//! BIOS card functions.
//!
//! **libmcx** talks to a PocketStation in a memory card slot over SIO0
//! itself. A command function (`McxGetApl`, `McxCardType`, …) reserves a
//! transfer in libmcx's state. At the next vblank libmcx's element in the
//! kernel's priority-2 chain sends the slot's address (81h, after a wait on
//! root counter 2); each /ACK's interrupt (IRQ7, its priority-1 element,
//! whose handler is `_mcxDsrEntry`) sends the next byte, which the
//! command's callbacks (`_mcxFuncGetTx`, `_mcxFuncApplyRx`) give and check.
//! A transfer still running at the next vblank has timed out. `McxSync`
//! reads the outcome from that state.
//!
//! **libmcx's state stays where libmcx keeps it** (D59's second option, as
//! libgpu's, D61): `_mcxStat`, `_mcxCmdSeq`, the buffers and lengths, the
//! callbacks. So the command functions, `McxSync` and `McxStopCom` stay
//! original guest code over it. The adapters are the two functions that
//! drive the hardware: `McxStartCom` (I_STAT and I_MASK, and its element
//! linked into the kernel's chain through the table of tables) and
//! `_mcxDsrEntry` (the transfer's bytes on SIO0). The two elements'
//! verifiers and the vblank handler run as adapter steps in the elements'
//! places ([`chain_element`]). Their addresses come from the objects'
//! relocated code (`mcxinit`, `mcxmain`, `mcxresi`, `mcxdsr`, validated, so
//! every operand is where PsyQ 4.7's objects have it).
//!
//! **The wire** ([`Wire`]) is what libmcx sees of SIO0: the slot selected,
//! the byte last received, the port's interrupt flag. A slot's card is the
//! BIOS's image, answering as SIO0's device's cards do
//! (`psx_devices::memcard::Responder`, a standard card as Beetle's is): 81h
//! with its FLAG and an /ACK, `R` and `W` as frame transfers, every
//! PocketStation command refused. There is no PocketStation: with a card,
//! a command gets the address's /ACK and no other (McxErrInvalid); with no
//! card, nothing (McxErrNoCard). `McxExecFlag` sends `R` and `W`, which a
//! card answers.
//!
//! **Time.** The adapters spend their originals' blocks on their paths
//! (D71), the verifiers on every interrupt the kernel walks the chains
//! for. In the exception each block is spent before what it does, so the
//! wait before the address byte (`_mcxSioRW`'s, on root counter 2) reads
//! the counter itself, pass by pass, at the cycle after the block that
//! reads it (D66), and ends at the pass that sees the 91h ticks; a quantum
//! passes only at a loop header, as libpad's do (D60). A byte's /ACK comes
//! at the guest's cycle it went out, plus the byte and the card's delay,
//! and raises IRQ7 in the interrupt controller at its time, as SIO0's
//! device does, which leaves it pending until the next interrupt the
//! machine takes walks the chains.
//!
//! **What isn't reproduced.** The kernel patch `_mcxStartFastDsr` makes
//! (its fast path, entered from the exception vector) isn't taken: the HLE
//! BIOS runs no exception code from RAM, so the bulk bytes reach
//! `_mcxFastDsrEnt` through `_mcxDsrEntry`, as the original's do when it is
//! called from C. Of the port's registers only the slot select, the
//! interrupt flag, the received byte and the baud (for the /ACK's time)
//! are kept.

use crate::{Adapter, M, Result, bios, reg, ret, set_reg};
use psx_bios::{Event, Flow, GuestCall, IrqStep};
use psx_devices::memcard::Responder;

/// libcard's functions that patch the Sony BIOS's card driver in kernel
/// RAM: the HLE BIOS has no such code, so there is nothing to patch.
pub const ADAPTERS: &[(&str, Adapter)] = &[
    ("_patch_card", zero),
    ("_patch_card2", zero),
    ("_patch_card_info", zero),
    ("_copy_memcard_patch", zero),
    ("_ExitCard", zero),
];

/// Chip memcard's adapters (libmcx).
pub const MCX: &[(&str, Adapter)] = &[("McxStartCom", start_com), ("_mcxDsrEntry", dsr_entry)];

/// The exception steps that run libmcx's elements: the vblank element
/// (`_mcxTblCardEnt`), the /ACK element (`_mcxTblDsrEnt`), and the vblank
/// handler's tail after its call of `_mcxDsrEntry`.
pub const STEP_CARD: u32 = 0x4D43_0001;
pub const STEP_DSR: u32 = 0x4D43_0002;
const STEP_START_TAIL: u32 = 0x4D43_0003;
/// The event a card's /ACK is (data: the wire's generation).
pub const EV_ACK: u32 = 0x4D43_0001;

const I_STAT: u32 = psx_devices::irq::I_STAT;
const I_MASK: u32 = psx_devices::irq::I_MASK;
/// IRQ7, SIO0's /ACK.
const ACK_LINE: u32 = 1 << psx_devices::irq::SIO0;

/// `mcxresi`'s `.data`: the two elements, then libmcx's state words.
mod data {
    pub const CARD_ENT: u32 = 0x10;
    pub const DSR_ENT: u32 = 0x20;
    pub const STAT: u32 = 0x30;
    pub const CMD_SEQ: u32 = 0x34;
    pub const PORT: u32 = 0x38;
    pub const PASS: u32 = 0x3C;
    pub const UIFS_RETRY: u32 = 0x40;
    pub const TX_LEN: u32 = 0x44;
    pub const RX_LEN: u32 = 0x48;
}

/// Offsets in `mcxresi`'s text: the elements' verifiers, the vblank
/// handler, `_mcxStepSio`.
mod text {
    pub const VERIFY_VBLANK: u32 = 0x0;
    pub const VERIFY_ACK: u32 = 0x40;
    pub const HANDLER: u32 = 0x80;
    pub const STEP_SIO: u32 = 0x278;
}

/// `mcxdsr`'s words for a bulk transfer, in its `.data`: the direction
/// (nonzero: to the card), the buffer, the checksum's address, the bytes
/// left (`_mcxIntRestSize`), `_mcxCallFromC`.
mod fast {
    pub const TO_CARD: u32 = 0x08;
    pub const BUFFER: u32 = 0x0C;
    pub const SUM: u32 = 0x10;
    pub const LEFT: u32 = 0x14;
    pub const FROM_C: u32 = 0x18;
}

/// `_mcxStat`: bit 0 idle; reserved, running; the ends (no /ACK in time,
/// a callback's error, done) are idle.
const IDLE_BIT: u32 = 1;
const RESERVED: u32 = 2;
const RUNNING: u32 = 4;
const TIMEOUT: u32 = 0x11;
const ERROR: u32 = 0x21;
const DONE: u32 = 0x41;
/// `McxFuncSetUIFS`: its timeouts retry twice.
const SET_UIFS: u32 = 17;
/// SIO0's JOY_BAUD once libmcx has reset the port.
const BAUD: u16 = 0x88;

/// Where libmcx is in this game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lib {
    /// `mcxresi`'s `.data`, and `_mcxDsrEntry`.
    pub data: u32,
    pub dsr_entry: u32,
    /// `mcxdsr`'s bulk-transfer words ([`fast`]).
    pub fast: u32,
    pub func_no: u32,
    pub rest: u32,
    pub tx_buf: u32,
    pub rx_buf: u32,
    pub get_tx: u32,
    pub apply_rx: u32,
    pub data_len: u32,
}

impl Lib {
    fn at(&self, off: u32) -> u32 {
        self.data + off
    }
}

/// What libmcx sees of SIO0.
#[derive(Debug, Default)]
pub struct Wire {
    /// The slot selected (DTR), and its card's side of the transfer.
    slot: Option<usize>,
    card: Responder,
    /// JOY_DATA: the reply to the last byte sent.
    rx: u8,
    /// JOY_STAT bit 9: an /ACK raised the port's interrupt, not yet
    /// acknowledged at the port.
    irq: bool,
    /// JOY_BAUD as libmcx leaves it (0 at power-on).
    baud: u16,
    /// Counts selections: an /ACK of an earlier one doesn't arrive.
    generation: u32,
    /// When the last byte went out, and the guest cycles of the /ACKs of
    /// it and of the one before (/ACK is low for 32 cycles).
    sent: Option<u64>,
    ack: Option<u64>,
    last_ack: Option<u64>,
}

#[derive(Debug, Default)]
pub struct Mc {
    pub lib: Option<Lib>,
    pub wire: Wire,
}

// --- Where libmcx is ----------------------------------------------------------

/// The address a `lui` at `hi` and the `lo16` instruction at `lo` make (the
/// latter's base register is the `lui`'s).
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

/// libmcx from `mcxresi`'s `.data`: the elements give the vblank handler's
/// and `_mcxDsrEntry`'s addresses, and the handler's and `_mcxStepSio`'s
/// code the rest.
fn locate(m: &mut M, data: u32) -> std::result::Result<Lib, String> {
    if let Some(l) = m.sdk.mc.lib.filter(|l| l.data == data) {
        return Ok(l);
    }
    let r = |m: &M, a: u32| m.mem.r32(a);
    let text = r(m, data + data::CARD_ENT + 4).wrapping_sub(text::HANDLER);
    let err = |what: &str| format!("libmcx/mcxresi at {text:#010x}: no {what} where PsyQ 4.7's object has it");
    if r(m, data + data::CARD_ENT + 8) != text + text::VERIFY_VBLANK || r(m, data + data::DSR_ENT + 8) != text + text::VERIFY_ACK {
        return Err(err("element verifiers"));
    }
    let dsr_entry = r(m, data + data::DSR_ENT + 4);
    let (h, s) = (text + text::HANDLER, text + text::STEP_SIO);
    if jal(m, h + 0x170) != Some(dsr_entry) {
        return Err(err("call of _mcxDsrEntry"));
    }
    let find = |m: &M, what: &str, hi: u32, lo: u32| pair(m, hi, lo).ok_or_else(|| err(what));
    let left = find(m, "_mcxIntRestSize", h + 0x110, h + 0x114)?;
    let l = Lib {
        data,
        dsr_entry,
        fast: left.wrapping_sub(fast::LEFT),
        func_no: find(m, "_mcxFuncNo", h + 0x18, h + 0x1C)?,
        tx_buf: find(m, "_mcxTxBuf", s + 0x70, s + 0x74)?,
        rest: find(m, "_mcxRestSize", s + 0x88, s + 0x8C)?,
        get_tx: find(m, "_mcxFuncGetTx", s + 0x90, s + 0x94)?,
        rx_buf: find(m, "_mcxRxBuf", s + 0xF0, s + 0xF8)?,
        apply_rx: find(m, "_mcxFuncApplyRx", s + 0xFC, s + 0x100)?,
        data_len: find(m, "_mcxDataLen", s + 0x168, s + 0x16C)?,
    };
    m.sdk.mc.lib = Some(l);
    Ok(l)
}

// --- The wire ---------------------------------------------------------------------
//
// libmcx's elements and `_mcxDsrEntry` run in the exception: they spend
// their originals' blocks as they go (offsets in `mcxresi`'s, `mcxmain`'s
// or `mcxdsr`'s text, D71), so SIO0 and root counter 2 are read at the
// cycle after the block that reads them (`Machine::guest_now`, D66), and
// a quantum passes only at a loop header.

/// The original's block at `a`..=`b`, in the exception.
fn at(m: &mut M, a: u32, b: u32) {
    crate::irq_path(m, &[(a, b)]);
}

/// JOY_CTRL 0: no slot selected; an /ACK not yet arrived is lost.
fn deselect(m: &mut M) {
    let w = &mut m.sdk.mc.wire;
    w.slot = None;
    w.generation = w.generation.wrapping_add(1);
    (w.ack, w.last_ack) = (None, None);
}

/// `_mcxInitSioMode(mode)` (`mcxmain` +144h): the wait before the address
/// started (`_mcxSetRC2wait(0x91)`), the slot selected (mode 0: slot 1), and
/// an interrupt pending at the port acknowledged, there and in I_STAT.
fn init_sio_mode(m: &mut M, mode: u32) -> std::result::Result<Wait, String> {
    at(m, 0x144, 0x164);
    deselect(m);
    let wait = Wait::set(m, 0x91);
    at(m, 0x168, 0x174);
    if mode != 0 {
        at(m, 0x178, 0x178);
    }
    at(m, 0x17C, 0x190);
    let w = &mut m.sdk.mc.wire;
    w.slot = Some((mode != 0) as usize);
    w.card.select();
    if std::mem::take(&mut w.irq) {
        at(m, 0x194, 0x1B4);
        at(m, 0x1F4, 0x204);
        set_reg(m, I_STAT, 4, !ACK_LINE)?;
    }
    at(m, 0x208, 0x214);
    Ok(wait)
}

/// A byte out to the selected slot: its card's answer comes in during it,
/// and an /ACK after it if the card answers the byte. The BIOS's cards are
/// loaded when a card's address first goes out, as on SIO0's device.
fn send(m: &mut M, b: u8) {
    if b == 0x81 {
        m.cards_ready();
    }
    let now = m.guest_now();
    let w = &mut m.sdk.mc.wire;
    (w.sent, w.last_ack) = (Some(now), w.ack.take());
    let Some(slot) = w.slot else {
        w.rx = 0xFF;
        return;
    };
    w.rx = w.card.next().unwrap_or(0xFF);
    if w.card.byte(psx_bios::card::slot_card(&mut m.io, slot), b).is_some() {
        let at = now + byte_time(w) + psx_devices::sio::CARD_ACK_DELAY as u64;
        w.ack = Some(at);
        let g = w.generation;
        m.clock.schedule(at, Event::Sdk(EV_ACK, g));
    }
}

/// A byte's time on the wire at the port's baud: eight bits.
fn byte_time(w: &Wire) -> u64 {
    8 * (w.baud as u64 & !1).max(0x20)
}

/// JOY_STAT: transmit ready, the byte received, /ACK low (32 cycles from
/// the card's /ACK).
fn tx_ready(m: &M) -> bool {
    let w = &m.sdk.mc.wire;
    w.sent.is_none_or(|s| m.guest_now() >= s + byte_time(w))
}

fn rx_ready(m: &M) -> bool {
    let w = &m.sdk.mc.wire;
    w.sent.is_some_and(|s| m.guest_now() >= s + byte_time(w))
}

fn ack_low(m: &M) -> bool {
    let t = m.guest_now();
    let w = &m.sdk.mc.wire;
    [w.ack, w.last_ack].into_iter().flatten().any(|a| a <= t && t < a + 32)
}

/// A card's /ACK: the port's interrupt raised (JOY_CTRL bit 12, which
/// libmcx always sets), IRQ7 in I_STAT. As SIO0's device's, it waits
/// there for the next interrupt the machine takes.
pub fn on_ack(m: &mut M, generation: u32) {
    let w = &mut m.sdk.mc.wire;
    if generation != w.generation || w.slot.is_none() {
        return;
    }
    w.irq = true;
    if let Some(io) = m.io.as_mut() {
        io.irq.raise(psx_devices::irq::SIO0);
    }
}

/// Root counter 2's counter; its mode and target follow, 4 and 8 on.
const RC2: u32 = 0x1F80_1120;

/// A wait on root counter 2 (`_mcxSetRC2wait`'s words): where it started,
/// its ticks, and the counter's wraps since, which its mode's flags say.
struct Wait {
    base: i32,
    ticks: i32,
    wraps: i32,
}

impl Wait {
    /// `_mcxSetRC2wait(ticks)` (+218h): the counter, then its mode, read.
    fn set(m: &mut M, ticks: i32) -> Self {
        at(m, 0x218, 0x248);
        let base = counter(m, 0);
        counter(m, 4);
        Wait { base, ticks, wraps: 0 }
    }

    /// The counter past `base`, after reading its mode then the counter at
    /// `read`'s block: a wrap at FFFFh adds 10000h (`wrap`), one at the
    /// target, where it resets there, the target (`target`; the test
    /// between is `test`); `join` stores them. True once `ticks` have
    /// passed (a cycle in 8 counted, or every cycle, its mode's bit 9;
    /// `fast` or `slow`, then `end`).
    fn over(&mut self, m: &mut M, [read, wrap, test, target, join, check, fast, slow, end]: [(u32, u32); 9]) -> bool {
        at(m, read.0, read.1);
        let mode = counter(m, 4);
        let now = counter(m, 0);
        if mode & 0x1000 != 0 {
            at(m, wrap.0, wrap.1);
            self.wraps += 0x10000;
            at(m, join.0, join.1);
        } else {
            at(m, test.0, test.1);
            if mode & 0x808 == 0x808 {
                at(m, target.0, target.1);
                self.wraps += counter(m, 8);
                at(m, join.0, join.1);
            }
        }
        at(m, check.0, check.1);
        let d = now + self.wraps - self.base;
        let over = if mode & 0x200 != 0 {
            at(m, fast.0, fast.1);
            d >= self.ticks
        } else {
            at(m, slow.0, slow.1);
            d >= self.ticks * 8
        };
        at(m, end.0, end.1);
        over
    }
}

fn counter(m: &mut M, r: u32) -> i32 {
    reg(m, RC2 + r, 2).unwrap_or(0) as i32
}

/// `_mcxSioRW(data)` (+31Ch), `data < 0`: the address (its complement)
/// out once the wait is over, polled a pass of its loop (+39Ch) at a time.
fn address(m: &mut M, mut wait: Wait, b: u8) {
    at(m, 0x31C, 0x33C);
    at(m, 0x340, 0x350);
    if !tx_ready(m) {
        at(m, 0x354, 0x354);
        loop {
            crate::irq_header(m);
            at(m, 0x358, 0x368);
            if tx_ready(m) {
                break;
            }
        }
    }
    at(m, 0x36C, 0x398);
    loop {
        crate::irq_header(m);
        let blocks = [(0x39C, 0x3B0), (0x3B4, 0x3C0), (0x3C4, 0x3CC), (0x3D0, 0x3E0), (0x3E4, 0x3EC), (0x3F0, 0x3F4), (0x3F8, 0x410), (0x414, 0x428), (0x42C, 0x434)];
        if wait.over(m, blocks) {
            break;
        }
    }
    at(m, 0x438, 0x448);
    send(m, b);
    at(m, 0x50C, 0x520);
}

/// `_mcxSioRW(data)`, `data >= 0`: the byte received for the last one
/// (JOY_DATA), the next sent, IRQ7 acknowledged, a wait for /ACK to rise
/// (60 ticks, `_mcxChkRC2wait`), then the port's acknowledged. -768 less
/// the byte if the wait runs out.
fn sio_rw(m: &mut M, b: u8) -> std::result::Result<i32, String> {
    at(m, 0x31C, 0x33C);
    let got = m.sdk.mc.wire.rx;
    at(m, 0x44C, 0x4A4);
    send(m, b);
    let mut wait = Wait { base: counter(m, 0), ticks: 60, wraps: 0 };
    counter(m, 4);
    set_reg(m, I_STAT, 4, !ACK_LINE)?;
    while ack_low(m) {
        crate::irq_header(m);
        at(m, 0x4A8, 0x4AC);
        let blocks = [(0x24C, 0x270), (0x274, 0x280), (0x284, 0x290), (0x294, 0x2A8), (0x2AC, 0x2B8), (0x2BC, 0x2C0), (0x2F0, 0x310), (0x2C4, 0x2EC), (0x314, 0x318)];
        let over = wait.over(m, blocks);
        at(m, 0x4B0, 0x4B4);
        if over {
            at(m, 0x4F4, 0x4F8);
            at(m, 0x4FC, 0x508);
            at(m, 0x514, 0x520);
            return Ok(-768 - got as i32);
        }
        at(m, 0x4B8, 0x4D4);
    }
    at(m, 0x4D8, 0x4F0);
    m.sdk.mc.wire.irq = false;
    at(m, 0x4F4, 0x4F8);
    at(m, 0x50C, 0x520);
    Ok(got as i32)
}

/// JOY_CTRL 40h, then mode 0Dh, baud 88h and control 0: the port reset.
fn reset_port(m: &mut M) {
    deselect(m);
    let w = &mut m.sdk.mc.wire;
    w.irq = false;
    w.rx = 0;
    w.baud = BAUD;
    w.sent = None;
}

// --- libmcx's state -------------------------------------------------------------

fn get(m: &M, a: u32) -> u32 {
    m.mem.r32(a)
}

fn put(m: &mut M, a: u32, v: u32) {
    m.mem.w32(a, v)
}

/// `_mcxChkRsrv(port)` (`mcxresi` +228h): an idle libmcx starts a
/// transfer's books.
fn chk_rsrv(m: &mut M, l: Lib, port: u32) -> bool {
    at(m, 0x228, 0x23C);
    let idle = get(m, l.at(data::STAT)) & IDLE_BIT != 0;
    if idle {
        at(m, 0x240, 0x268);
        for a in [l.at(data::TX_LEN), l.at(data::RX_LEN), l.rest, l.at(data::CMD_SEQ)] {
            put(m, a, 0);
        }
        put(m, l.at(data::PORT), port);
    } else {
        at(m, 0x26C, 0x26C);
    }
    at(m, 0x270, 0x274);
    idle
}

/// A transfer's end: the port deselected, IRQ7 acknowledged and masked.
fn quiet(m: &mut M) -> std::result::Result<(), String> {
    deselect(m);
    set_reg(m, I_STAT, 4, !ACK_LINE)?;
    let mask = reg(m, I_MASK, 4)?;
    set_reg(m, I_MASK, 4, mask & !ACK_LINE)
}

// --- The adapters -------------------------------------------------------------------

/// `McxStartCom()`: libmcx's vblank element (`_mcxTblCardEnt`) taken out of
/// priority 2's chain and appended at its end through the table of
/// tables, as `mcxinit` does (+B0h, a pass of its loop an element);
/// vblank acknowledged and unmasked; root counter 3's events no longer
/// cleared (`ChangeClearRCnt(3, 0)`); all in a critical section (v0:
/// ExitCriticalSection's 0).
fn start_com(m: &mut M, _a: [u32; 4]) -> Result {
    let at = m.sdk.at;
    let err = |what: &str| format!("libmcx/mcxinit at {at:#010x}: no {what} where PsyQ 4.7's object has it");
    let card_ent = pair(m, at + 0x14, at + 0x18).ok_or_else(|| err("_mcxTblCardEnt"))?;
    let tables = pair(m, at + 0xB0, at + 0xB4).ok_or_else(|| err("table of tables"))?;
    locate(m, card_ent.wrapping_sub(data::CARD_ENT))?;
    crate::path(m, &[(0x0, 0xC)]);
    crate::take(m, crate::SYSCALL_STUB);
    psx_bios::bios::syscall(m, [1, 0, 0, 0])?;
    crate::path(m, &[(0x10, 0x20)]);
    crate::take(m, crate::VECTOR_STUB);
    bios(m, 0xC0, 0x03, [2, card_ent, 0, 0])?;
    crate::path(m, &[(0x24, 0x2C), (0xB0, 0xD4)]);
    let head = get(m, get(m, tables)).wrapping_add(2 * 8);
    let mut passes = 0;
    match get(m, head) {
        0 => {
            crate::path(m, &[(0xD8, 0xDC)]);
            put(m, head, card_ent);
        }
        mut e => {
            passes = 1;
            while get(m, e) != 0 {
                e = get(m, e);
                passes += 1;
            }
            put(m, e, card_ent);
        }
    }
    put(m, card_ent, 0);
    Ok(crate::libspu::each(
        m,
        0,
        passes,
        4,
        std::rc::Rc::new(move |m, i| {
            if i + 1 < passes {
                crate::path(m, &[(0xE0, 0xE4)]);
            }
        }),
        Box::new(move |m| {
            if passes > 0 {
                crate::path(m, &[(0xF8, 0xF8)]);
            }
            crate::path(m, &[(0xFC, 0x100), (0x30, 0x54)]);
            let r = (|| {
                set_reg(m, I_STAT, 4, !1)?;
                let mask = reg(m, I_MASK, 4)?;
                set_reg(m, I_MASK, 4, mask | 1)?;
                crate::take(m, crate::VECTOR_STUB);
                bios(m, 0xC0, 0x0A, [3, 0, 0, 0])?;
                crate::path(m, &[(0x58, 0x5C)]);
                crate::take(m, crate::SYSCALL_STUB);
                psx_bios::bios::syscall(m, [2, 0, 0, 0])
            })();
            if let Err(e) = r {
                m.log(&format!("libmcx: {e}"));
            }
            crate::path(m, &[(0x60, 0x6C)]);
            Flow::ret(0)
        }),
    ))
}

/// What follows a step of `_mcxStepSio`, with its result.
type Then = Box<dyn FnOnce(&mut M, i32) -> Result>;

/// `_mcxDsrEntry()` (`mcxmain` +0h): a bulk byte (`_mcxFastDsrEnt`, called
/// from C), or the transfer's next step (`_mcxStepSio`) and its end: done
/// (and the second pass reserved, for `McxExecFlag`), or a callback's error.
fn dsr_entry(m: &mut M, _a: [u32; 4]) -> Result {
    let at = m.sdk.at;
    let dsr_ent = pair(m, at + 0xD4, at + 0xD8).ok_or_else(|| format!("libmcx/mcxmain at {at:#010x}: no _mcxTblDsrEnt where PsyQ 4.7's object has it"))?;
    let l = locate(m, dsr_ent.wrapping_sub(data::DSR_ENT))?;
    if l.dsr_entry != at {
        return Err(format!("libmcx: _mcxTblDsrEnt's handler is {:#010x}, not _mcxDsrEntry at {at:#010x}", l.dsr_entry));
    }
    self::at(m, 0x0, 0x10);
    if get(m, l.fast + fast::LEFT) != 0 {
        self::at(m, 0x14, 0x20);
        put(m, l.fast + fast::FROM_C, 1);
        fast_dsr(m, l)?;
        self::at(m, 0x24, 0x30);
        put(m, l.fast + fast::FROM_C, 0);
        self::at(m, 0x134, 0x140);
        return ret(0);
    }
    self::at(m, 0x34, 0x4C);
    let seq = get(m, l.at(data::CMD_SEQ)) + 1;
    put(m, l.at(data::CMD_SEQ), seq);
    step_sio(m, l, Box::new(move |m, r| finish(m, l, r)))
}

/// After `_mcxStepSio` (+50h): 0 waits for the next /ACK; 1 is done; anything
/// else is an error.
fn finish(m: &mut M, l: Lib, r: i32) -> Result {
    at(m, 0x50, 0x58);
    if r == 0 {
        at(m, 0x130, 0x140);
        return ret(0);
    }
    at(m, 0x5C, 0x60);
    if r == 1 {
        at(m, 0x64, 0x7C);
        put(m, l.at(data::STAT), DONE);
        let pass = get(m, l.at(data::PASS));
        if pass != 0 {
            at(m, 0x80, 0x8C);
            let port = get(m, l.at(data::PORT));
            chk_rsrv(m, l, port);
            at(m, 0x90, 0xB0);
            put(m, l.at(data::STAT), RESERVED);
            put(m, l.at(data::PASS), pass - 1);
        }
        at(m, 0xB4, 0xE4);
        quiet(m)?;
    } else {
        at(m, 0xE8, 0x120);
        quiet(m)?;
        put(m, l.at(data::STAT), ERROR);
    }
    at(m, 0x124, 0x12C);
    stub(m);
    put(m, l.at(data::CMD_SEQ), 0);
    bios(m, 0xC0, 0x03, [1, l.at(data::DSR_ENT), 0, 0])?;
    at(m, 0x130, 0x140);
    ret(0)
}

/// A guest call (the original's `jalr`), then `then` with its result.
fn call(addr: u32, a0: u32, then: impl FnOnce(&mut M, i32) -> Result + 'static) -> Result {
    Ok(Flow::Call {
        call: GuestCall { addr, args: [a0, 0, 0, 0] },
        then: Box::new(move |m, v| match then(m, v as u32 as i32) {
            Ok(f) => f,
            Err(e) => {
                m.log(&format!("libmcx: {e}"));
                Flow::ret(0)
            }
        }),
    })
}

/// `_mcxStepSio()` (`mcxresi` +278h): at step 1 the slot's address, after
/// the wait; then a byte from the command's callback out, and the byte
/// received for the last one to its other callback; with one byte left,
/// that one, read once it has arrived. `k` gets the result.
fn step_sio(m: &mut M, l: Lib, k: Then) -> Result {
    at(m, 0x278, 0x290);
    if get(m, l.at(data::CMD_SEQ)) == 1 {
        at(m, 0x294, 0x2AC);
        put(m, l.at(data::STAT), RUNNING);
        let port = get(m, l.at(data::PORT));
        let wait = init_sio_mode(m, ((port as i32) >> 4) as u32)?;
        at(m, 0x2B0, 0x2C8);
        address(m, wait, (port as u8 & 0xF).wrapping_add(0x81));
        at(m, 0x2CC, 0x2FC);
        m.mem.w8(l.tx_buf, (port as u8 & 0xF).wrapping_sub(0x7F));
        let n = get(m, l.at(data::TX_LEN));
        put(m, l.at(data::TX_LEN), n + 1);
        at(m, 0x494, 0x4A0);
        return k(m, 0);
    }
    at(m, 0x300, 0x31C);
    let rest = get(m, l.rest).wrapping_sub(1);
    put(m, l.rest, rest);
    let get_tx = get(m, l.get_tx);
    call(get_tx, 0, move |m, tx| {
        at(m, 0x320, 0x344);
        let tx = tx as u8;
        let n = get(m, l.at(data::TX_LEN));
        m.mem.w8(l.tx_buf + n, tx);
        put(m, l.at(data::TX_LEN), n + 1);
        let got = sio_rw(m, tx)?;
        at(m, 0x348, 0x350);
        if got < 0 {
            at(m, 0x354, 0x358);
            at(m, 0x494, 0x4A0);
            return k(m, got - 1000);
        }
        received(m, l, got as u8, k)
    })
}

/// A received byte stored and given to `_mcxFuncApplyRx` (+35Ch), and what
/// follows.
fn received(m: &mut M, l: Lib, b: u8, k: Then) -> Result {
    at(m, 0x35C, 0x384);
    let n = get(m, l.at(data::RX_LEN));
    m.mem.w8(l.rx_buf + n, b);
    put(m, l.at(data::RX_LEN), n + 1);
    let apply = get(m, l.apply_rx);
    call(apply, b as u32, move |m, r| {
        at(m, 0x388, 0x390);
        if r != 0 {
            at(m, 0x494, 0x4A0);
            return k(m, r - 2000);
        }
        at(m, 0x394, 0x3A4);
        let r = match get(m, l.rest) as i32 {
            0 => {
                at(m, 0x3A8, 0x3AC);
                1
            }
            1 => {
                at(m, 0x3B0, 0x3B4);
                return last(m, l, k);
            }
            _ => {
                at(m, 0x3B0, 0x3B4);
                0
            }
        };
        at(m, 0x494, 0x4A0);
        k(m, r)
    })
}

/// The transfer's last byte (+3B8h), read once it has arrived, with no
/// /ACK to wait for: `ReadDev` and `WriteDev` don't wait when all the rest
/// is in.
fn last(m: &mut M, l: Lib, k: Then) -> Result {
    at(m, 0x3B8, 0x3C8);
    let f = get(m, l.func_no);
    let check = f == 14 || {
        at(m, 0x3CC, 0x3DC);
        f == 15
    };
    let all = check && {
        at(m, 0x3E0, 0x3F8);
        get(m, l.at(data::RX_LEN)) == get(m, l.data_len) + 4
    };
    if !all {
        at(m, 0x3FC, 0x404);
        loop {
            crate::irq_header(m);
            at(m, 0x408, 0x418);
            if rx_ready(m) {
                break;
            }
        }
    }
    at(m, 0x41C, 0x480);
    let seq = get(m, l.at(data::CMD_SEQ)) + 1;
    put(m, l.at(data::CMD_SEQ), seq);
    let rest = get(m, l.rest).wrapping_sub(1);
    put(m, l.rest, rest);
    let b = m.sdk.mc.wire.rx;
    let n = get(m, l.at(data::RX_LEN));
    m.mem.w8(l.rx_buf + n, b);
    put(m, l.at(data::RX_LEN), n + 1);
    let apply = get(m, l.apply_rx);
    call(apply, b as u32, move |m, r| {
        at(m, 0x484, 0x48C);
        if r != 0 {
            at(m, 0x490, 0x490);
        }
        at(m, 0x494, 0x4A0);
        k(m, if r != 0 { r - 3000 } else { 1 })
    })
}

/// `_mcxFastDsrEnt` (`mcxdsr` +1C0h), called from C: with IRQ7 pending, one
/// bulk byte, to the card from the buffer or from the card into it, XORed
/// into the checksum. (Its return without one runs `_mcxVecSave`'s words,
/// lifted as they are in the object.)
fn fast_dsr(m: &mut M, l: Lib) -> std::result::Result<(), String> {
    at(m, 0x1C0, 0x1D0);
    let left = get(m, l.fast + fast::LEFT);
    let pending = left != 0 && {
        at(m, 0x1D4, 0x1E8);
        reg(m, I_STAT, 4)? & ACK_LINE != 0
    };
    if !pending {
        at(m, 0x2CC, 0x2F4);
        return Ok(());
    }
    at(m, 0x1EC, 0x1FC);
    let buf = get(m, l.fast + fast::BUFFER);
    let v = if get(m, l.fast + fast::TO_CARD) != 0 {
        at(m, 0x200, 0x228);
        let b = m.mem.r8(buf);
        send(m, b);
        b
    } else {
        at(m, 0x22C, 0x248);
        let b = m.sdk.mc.wire.rx;
        send(m, 0);
        m.mem.w8(buf, b);
        b
    };
    at(m, 0x24C, 0x2C0);
    put(m, l.fast + fast::BUFFER, buf + 1);
    set_reg(m, I_STAT, 4, !ACK_LINE)?;
    m.sdk.mc.wire.irq = false;
    let sum = get(m, l.fast + fast::SUM);
    let s = m.mem.r8(sum) ^ v;
    m.mem.w8(sum, s);
    put(m, l.fast + fast::LEFT, left - 1);
    let seq = get(m, l.at(data::CMD_SEQ)) + 1;
    put(m, l.at(data::CMD_SEQ), seq);
    at(m, 0x2F0, 0x2F4);
    Ok(())
}

// --- The elements -----------------------------------------------------------------

/// libmcx's elements, once its adapters have found it: the kernel runs
/// their steps in their places.
pub fn chain_element(m: &M, elem: u32) -> Option<u32> {
    let l = m.sdk.mc.lib?;
    match elem.wrapping_sub(l.data) {
        data::CARD_ENT => Some(STEP_CARD),
        data::DSR_ENT => Some(STEP_DSR),
        _ => None,
    }
}

pub fn owns(step: u32) -> bool {
    matches!(step, STEP_CARD | STEP_DSR | STEP_START_TAIL)
}

/// An element's verifier (`mcxresi` +0h for vblank, +40h for the /ACK) on
/// every walk of the chains: interrupt line `bit` unmasked and pending.
fn verify(m: &mut M, base: u32, bit: u32) -> bool {
    at(m, base, base + 0x1C);
    let yes = reg(m, I_MASK, 4).is_ok_and(|v| v & bit != 0) && {
        at(m, base + 0x20, base + 0x30);
        let p = reg(m, I_STAT, 4).is_ok_and(|v| v & bit != 0);
        if !p {
            at(m, base + 0x34, base + 0x34);
        }
        p
    };
    at(m, base + 0x38, base + 0x3C);
    yes
}

pub fn irq_step(m: &mut M, step: u32) {
    let Some(l) = m.sdk.mc.lib else { return };
    let r = match step {
        STEP_CARD if verify(m, text::VERIFY_VBLANK, 1) => card_handler(m, l),
        // The /ACK element's handler is `_mcxDsrEntry`, an adapter: called
        // as the kernel calls it, with its verifier's 1.
        STEP_DSR if verify(m, text::VERIFY_ACK, ACK_LINE) => {
            m.irq_calls.push_front(IrqStep::Chain(GuestCall { addr: l.dsr_entry, args: [1, 0, 0, 0] }, None));
            Ok(())
        }
        STEP_START_TAIL => {
            at(m, 0x1F8, 0x214);
            let r = set_reg(m, I_STAT, 4, !ACK_LINE).and_then(|_| {
                let mask = reg(m, I_MASK, 4)?;
                set_reg(m, I_MASK, 4, mask | ACK_LINE)
            });
            at(m, 0x218, 0x224);
            r
        }
        _ => Ok(()),
    };
    if let Err(e) = r {
        m.log(&format!("libmcx: {e}"));
    }
}

/// The vblank handler (`mcxresi` +80h): a transfer still running at the
/// vblank has timed out (`SetUIFS`'s retries twice); a reserved one starts:
/// the /ACK element into priority 1's chain, `_mcxDsrEntry` called for the
/// first step, then IRQ7 acknowledged and unmasked.
fn card_handler(m: &mut M, l: Lib) -> std::result::Result<(), String> {
    at(m, 0x80, 0x94);
    if get(m, l.at(data::CMD_SEQ)) != 0 {
        at(m, 0x98, 0xA8);
        let retry = get(m, l.func_no) == SET_UIFS && {
            at(m, 0xAC, 0xCC);
            let n = get(m, l.at(data::UIFS_RETRY)) + 1;
            put(m, l.at(data::UIFS_RETRY), n);
            n < 3
        };
        put(m, l.at(data::STAT), TIMEOUT);
        if retry {
            at(m, 0xD0, 0xE0);
            let port = get(m, l.at(data::PORT));
            chk_rsrv(m, l, port);
            at(m, 0xE4, 0x12C);
            put(m, l.at(data::STAT), RESERVED);
            quiet(m)?;
            put(m, l.at(data::CMD_SEQ), 0);
            stub(m);
            bios(m, 0xC0, 0x03, [1, l.at(data::DSR_ENT), 0, 0])?;
            at(m, 0x130, 0x134);
        } else {
            at(m, 0x138, 0x180);
            put(m, l.at(data::CMD_SEQ), 0);
            quiet(m)?;
            put(m, l.at(data::UIFS_RETRY), 0);
            stub(m);
            bios(m, 0xC0, 0x03, [1, l.at(data::DSR_ENT), 0, 0])?;
            at(m, 0x184, 0x1B4);
            put(m, l.fast + fast::LEFT, 0);
            reset_port(m);
            at(m, 0x218, 0x224);
            return Ok(());
        }
    }
    at(m, 0x1B8, 0x1CC);
    if get(m, l.at(data::STAT)) & IDLE_BIT == 0 {
        at(m, 0x1D0, 0x1E0);
        stub(m);
        bios(m, 0xC0, 0x03, [1, l.at(data::DSR_ENT), 0, 0])?;
        at(m, 0x1E4, 0x1EC);
        stub(m);
        bios(m, 0xC0, 0x02, [1, l.at(data::DSR_ENT), 0, 0])?;
        at(m, 0x1F0, 0x1F4);
        m.irq_calls.push_front(IrqStep::Sdk(STEP_START_TAIL));
        m.irq_calls.push_front(IrqStep::Chain(GuestCall { addr: l.dsr_entry, args: [0; 4] }, None));
        return Ok(());
    }
    at(m, 0x218, 0x224);
    Ok(())
}

/// libapi's stub of a BIOS vector call, in the exception.
fn stub(m: &mut M) {
    crate::irq_path(m, &[(0, (crate::VECTOR_STUB as u32 - 1) * 4)]);
}

// --- libcard ------------------------------------------------------------------------

fn zero(_m: &mut M, _a: [u32; 4]) -> Result {
    ret(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{call as run, machine_with_bus};

    /// libmcx in a game: `mcxinit`'s `McxStartCom`, `mcxmain`'s
    /// `_mcxDsrEntry`, `mcxresi`'s text and data, `mcxdsr`'s words and the
    /// common blocks, with the relocated operands the adapters read.
    const START_COM: u32 = 0x8004_5254;
    const DSR_ENTRY: u32 = 0x8004_5364;
    const RESI: u32 = 0x8004_4DA4;
    const DATA: u32 = 0x8005_E114;
    const TABLES: u32 = 0x8005_E200;
    const FAST: u32 = 0x8005_E300;
    const COMMON: u32 = 0x8006_0000;
    const FUNC_NO: u32 = COMMON;
    const REST: u32 = COMMON + 4;
    const GET_TX: u32 = COMMON + 8;
    const APPLY_RX: u32 = COMMON + 0xC;
    const DATA_LEN: u32 = COMMON + 0x10;
    const TX_BUF: u32 = COMMON + 0x20;
    const RX_BUF: u32 = COMMON + 0x100;

    /// A `lui` at `pc` and a `lo16` instruction at `lo` with the `lui`'s
    /// register as base.
    fn operand(m: &mut M, pc: u32, lo: u32, addr: u32) {
        let hi = addr.wrapping_add(0x8000) >> 16;
        m.mem.w32(pc, 0x3C01_0000 | hi);
        m.mem.w32(lo, 0x8C21_0000 | (addr & 0xFFFF));
    }

    fn game() -> M {
        let mut m = machine_with_bus();
        operand(&mut m, START_COM + 0x14, START_COM + 0x18, DATA + data::CARD_ENT);
        operand(&mut m, START_COM + 0xB0, START_COM + 0xB4, TABLES);
        m.mem.w32(TABLES, 0x100);
        operand(&mut m, DSR_ENTRY + 0xD4, DSR_ENTRY + 0xD8, DATA + data::DSR_ENT);
        let h = RESI + text::HANDLER;
        let s = RESI + text::STEP_SIO;
        for (hi, lo, addr) in [
            (h + 0x18, h + 0x1C, FUNC_NO),
            (h + 0x110, h + 0x114, FAST + fast::LEFT),
            (s + 0x70, s + 0x74, TX_BUF),
            (s + 0x88, s + 0x8C, REST),
            (s + 0x90, s + 0x94, GET_TX),
            (s + 0xF0, s + 0xF8, RX_BUF),
            (s + 0xFC, s + 0x100, APPLY_RX),
            (s + 0x168, s + 0x16C, DATA_LEN),
        ] {
            operand(&mut m, hi, lo, addr);
        }
        m.mem.w32(h + 0x170, 0x0C00_0000 | (DSR_ENTRY >> 2) & 0x03FF_FFFF);
        for (elem, handler, verifier) in [(data::CARD_ENT, h, RESI + text::VERIFY_VBLANK), (data::DSR_ENT, DSR_ENTRY, RESI + text::VERIFY_ACK)] {
            m.mem.w32(DATA + elem + 4, handler);
            m.mem.w32(DATA + elem + 8, verifier);
        }
        m.mem.w32(DATA + data::STAT, IDLE_BIT);
        m.mem.w32(GET_TX, 0x8007_0000);
        m.mem.w32(APPLY_RX, 0x8007_0100);
        m
    }

    fn adapter(m: &mut M, f: Adapter, at: u32) -> u32 {
        m.sdk.at = at;
        run(m, f, [0; 4])
    }

    /// A command reserved, as `McxCardType(port)` reserves it.
    fn reserve(m: &mut M, port: u32) {
        let l = m.sdk.mc.lib.unwrap();
        assert!(chk_rsrv(m, l, port));
        put(m, FUNC_NO, 0x13);
        put(m, DATA + data::STAT, RESERVED);
    }

    /// The vblank element's step with vblank pending, returning the guest
    /// calls it queued.
    fn vblank_step(m: &mut M) -> Vec<IrqStep> {
        set_reg(m, I_STAT, 4, 0).unwrap();
        m.io.as_mut().unwrap().irq.raise(psx_devices::irq::VBLANK);
        irq_step(m, STEP_CARD);
        m.irq_calls.drain(..).collect()
    }

    #[test]
    fn start_com_links_its_element_through_the_table_of_tables() {
        let mut m = game();
        assert_eq!(adapter(&mut m, start_com, START_COM), 0);
        let block = m.mem.r32(0x100) + 2 * 8;
        assert_eq!(m.mem.r32(block), DATA + data::CARD_ENT);
        assert_eq!(chain_element(&m, DATA + data::CARD_ENT), Some(STEP_CARD));
        assert_eq!(chain_element(&m, DATA + data::DSR_ENT), Some(STEP_DSR));
        assert_eq!(reg(&mut m, I_MASK, 4).unwrap() & 1, 1);
        assert_eq!(m.kernel.clear_rcnt[3], 0);
        // Again: taken out and appended once more, not twice.
        adapter(&mut m, start_com, START_COM);
        assert_eq!((m.mem.r32(block), m.mem.r32(DATA + data::CARD_ENT)), (DATA + data::CARD_ENT, 0));
    }

    /// Slot 1's card acknowledges the address, its /ACK's interrupt sends
    /// the command byte, and the next vblank times the transfer out with
    /// two bytes sent: McxErrInvalid. Slot 2 has none: one byte.
    #[test]
    fn a_command_to_each_slot() {
        for (port, sent) in [(0x00, 2), (0x10, 1)] {
            let mut m = game();
            adapter(&mut m, start_com, START_COM);
            reserve(&mut m, port);
            let steps = vblank_step(&mut m);
            assert_eq!(steps[0], IrqStep::Chain(GuestCall { addr: DSR_ENTRY, args: [0; 4] }, None));
            assert_eq!(steps[1], IrqStep::Sdk(STEP_START_TAIL));
            let t = m.clock.now;
            adapter(&mut m, dsr_entry, DSR_ENTRY);
            // The address byte waits 91h ticks of root counter 2 from _mcxSetRC2wait,
            // polled a pass at a time (D66, D71); slot 2 selects with one more
            // instruction. This machine has no guest budget: the passes are its time.
            assert_eq!(m.clock.now - t, [1382, 1384][(port >> 4) as usize], "the address byte's wait");
            irq_step(&mut m, STEP_START_TAIL);
            assert_eq!((get(&m, DATA + data::STAT), get(&m, DATA + data::TX_LEN), m.mem.r8(TX_BUF)), (RUNNING, 1, 0x81 + (port as u8 & 0xF)));
            assert_eq!(m.chain(1)[0], DATA + data::DSR_ENT, "at the head of priority 1, before the kernel's");
            m.step(m.clock.now + 5_000);
            let acked = reg(&mut m, I_STAT, 4).unwrap() & ACK_LINE != 0;
            assert_eq!(acked, sent == 2, "the card's /ACK");
            if acked {
                irq_step(&mut m, STEP_DSR);
                assert_eq!(m.irq_calls.pop_front(), Some(IrqStep::Chain(GuestCall { addr: DSR_ENTRY, args: [1, 0, 0, 0] }, None)));
                adapter(&mut m, dsr_entry, DSR_ENTRY);
                assert_eq!((m.mem.r8(RX_BUF), get(&m, DATA + data::RX_LEN)), (0xFF, 1), "the address's answer");
            }
            vblank_step(&mut m);
            assert_eq!((get(&m, DATA + data::STAT), get(&m, DATA + data::TX_LEN)), (TIMEOUT, sent));
            assert_eq!(reg(&mut m, I_MASK, 4).unwrap() & ACK_LINE, 0);
            assert!(!m.chain(1).contains(&(DATA + data::DSR_ENT)));
        }
    }
}
