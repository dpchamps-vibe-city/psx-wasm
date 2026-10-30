//! libcd (chip cd, D62): its drive layer as adapters over the CD
//! controller's core, without the controller's register protocol.
//!
//! **The seam.** PsyQ's libcd has two layers. `bios_1`, `bios_2` and
//! `bios_3` are the drive layer: `CD_init`, `CD_cw` (a command), `CD_sync`
//! and `CD_ready` (its completion and data interrupts), `CD_flush`,
//! `CD_vol`, `CD_initvol`, `CD_datasync`, `CD_getsector` and
//! `CD_getsector2` (DMA3 from the sector buffer), and the interrupt handler
//! `CD_init` installs on line 2. libcd's public layer (`CdInit`,
//! `CdControl*`, `CdSync`, `CdReady`, `CdRead*`, `CdSearchFile`, the
//! callback setters) and all of libds are code over that layer, and so is
//! the streaming ring, except its interrupt (`StCdInterrupt`) and
//! `StUnSetRing`, which reach the controller and DMA3 themselves. Those
//! are the adapters; everything above them runs as original code, in
//! every game: btn calls the public layer, bof4 libds and the ring.
//!
//! **State where the original keeps it.** libds writes the drive layer's
//! callback slots itself (`DS_init`, `DS_close`), the public layer reads its
//! status, mode and last command (`CdStatus`, `CdMode`, `CdLastCom`,
//! `CdLastPos`), and the callbacks get pointers to its result buffers. So
//! the adapters keep that state in `bios_1`'s own `.data` and `.bss`, and
//! the ring's in its own globals: every accessor stays original. An
//! adapter is told the guest address it replaces (`Psyq::at`, D61), and its
//! object's layout is fixed by the identity key it is validated for, so
//! the addresses come from the object's own relocated code ([`Bios1`],
//! [`Ring`]).
//!
//! **The controller** is `Machine::io`'s CD controller (D58), driven
//! through its core calls ([`Cdc::issue`], [`Cdc::pending`] …), never its
//! ports: one controller with one timing whether libcd runs native or as
//! original code on the ports (`--device cd`). Its interrupt is IRQ2, and
//! the handler on line 2 is a stand-in function ([`HANDLER`]) that
//! `CD_init` installs through the guest's own `InterruptCallback`, so
//! libetc dispatches it whichever way libetc runs. Sector transfers go
//! through DMA3 on the DMA controller the chips share, so DMA3's callback
//! (`CdDataCallback`, the ring's frame callback) arrives as on the device.
//! XA audio is the controller's (its decoder, into the SPU's CD input).
//!
//! **Waits spin,** as the original's loops do: a safepoint quantum at a
//! time, interrupts taken between quanta (D59's rule for adapters that
//! replace a loop). Inside a callback, where no interrupt is taken, a wait
//! polls the controller and calls the callbacks itself, as the original
//! does when `CheckCallback()` says so. Past 0x3C0000 passes a wait gives
//! up as the original does ("CD timeout", `CD_flush`, -1).
//!
//! **Not reproduced:** the debug printouts (`CD_debug`), and the memory
//! controller's delay registers the original sets around transfers (they
//! have no effect here).

use crate::libetc::{LOOP_BUDGET, quantum};
use crate::{Adapter, M, Psyq, Result, reg, ret, set_reg};
use psx_bios::{Flow, GuestCall, StandIn, Wait};
use psx_devices::cdc::Cdc;

pub const ADAPTERS: &[(&str, Adapter)] = &[
    ("CD_init", cd_init),
    ("CD_initintr", cd_initintr),
    ("CD_cw", cd_cw),
    ("CD_sync", cd_sync),
    ("CD_ready", cd_ready),
    ("CD_flush", cd_flush),
    ("CD_vol", cd_vol),
    ("CD_initvol", cd_initvol),
    ("CD_datasync", cd_datasync),
    ("CD_getsector", cd_getsector),
    ("CD_getsector2", cd_getsector2),
    ("StCdInterrupt", st_cd_interrupt),
    ("StUnSetRing", st_unset_ring),
];

/// The stand-in for `bios_1`'s interrupt handler, which `CD_init` and
/// `CD_initintr` install on line 2 (next to libetc's, [`crate::libetc`]).
pub const HANDLER: u32 = 0xBFC7_0030;

/// libcd command numbers the adapters issue themselves.
pub mod com {
    pub const NOP: u8 = 1;
    pub const SETLOC: u8 = 2;
    pub const INIT: u8 = 10;
    pub const DEMUTE: u8 = 12;
    pub const SETMODE: u8 = 14;
}

/// Offsets of `bios_1`'s functions in its text (PsyQ 4.7's object).
pub mod at {
    pub const CD_SYNC: u32 = 0x564;
    pub const CD_READY: u32 = 0x7E4;
    pub const CD_CW: u32 = 0xAAC;
    pub const CD_VOL: u32 = 0xEB8;
    pub const CD_FLUSH: u32 = 0xF40;
    pub const CD_INITVOL: u32 = 0x1014;
    pub const CD_INITINTR: u32 = 0x1104;
    pub const CD_INIT: u32 = 0x1150;
    pub const CD_DATASYNC: u32 = 0x1330;
}

/// `bios_1`'s `.data`, by offset.
mod d {
    pub const CBSYNC: u32 = 0x08;
    pub const CBREADY: u32 = 0x0C;
    pub const STATUS: u32 = 0x14;
    pub const STATUS1: u32 = 0x18;
    pub const NOPEN: u32 = 0x1C;
    pub const POS: u32 = 0x20;
    pub const MODE: u32 = 0x24;
    pub const COM: u32 = 0x25;
    /// Per command: its INT3 only acknowledges, INT2 completes it.
    pub const SECOND: u32 = 0xCC;
    /// Per command: issuing it clears the data-ready state.
    pub const READY_RESET: u32 = 0x14C;
    /// Per command: its INT3 response carries the drive status.
    pub const ACK_STATUS: u32 = 0x1CC;
    /// Per command: its parameter count.
    pub const NPARAMS: u32 = 0x24C;
    /// The interrupt states: sync (0 waiting, 2 complete, 3 acknowledged,
    /// 5 error), ready (1 data, 4 data end, 5 error), data end (4).
    pub const INTR: u32 = 0x2E4;
    /// What `CD_init` prints as its address.
    pub const INIT_ADDR: u32 = 0x2E8;
}

/// `bios_1`'s `.bss`: the result buffers the callbacks get.
mod bs {
    pub const SYNC: u32 = 0x00;
    pub const READY: u32 = 0x08;
    pub const DATA: u32 = 0x10;
}

/// Passes of a libcd wait loop before it gives up.
const TIMEOUT_PASSES: u64 = 0x3C_0000;

const DPCR: u32 = psx_devices::dma::DPCR;
const DICR: u32 = psx_devices::dma::DICR;
/// DMA channel 3: the CD controller's data FIFO.
const CH: u32 = 3;
const MADR3: u32 = psx_devices::dma::BASE + 0x10 * CH;
const BCR3: u32 = MADR3 + 4;
const CHCR3: u32 = MADR3 + 8;
const CHCR_BUSY: u32 = psx_devices::dma::CHCR_BUSY;
/// The SPU's registers `CD_initvol` sets.
const SPU: u32 = 0x1F80_1C00;

/// A finished read, for the run report's load timing (M7 task 7). The
/// drive layer has no notion of a load, so none are recorded now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Load {
    pub lba: u32,
    pub sectors: u32,
    /// Cycles from the read to its last sector.
    pub cycles: u64,
    /// Of which seeking.
    pub seek: u64,
}

#[derive(Debug, Default)]
pub struct Cd {
    /// Where `bios_1` is, as its adapters last found it (the line-2
    /// stand-in has no address of its own).
    pub bios1: Option<Bios1>,
    /// For the run report; the controller counts sectors and EDC errors.
    pub sectors: u64,
    pub edc_errors: Vec<String>,
    pub loads: Vec<Load>,
}

// --- Finding the library's state ------------------------------------------

/// The address a `lui`/low-16 instruction pair builds (its relocation).
fn hi_lo(m: &M, hi: u32, lo: u32) -> std::result::Result<u32, String> {
    let (h, l) = (m.mem.r32(hi), m.mem.r32(lo));
    if h >> 26 != 0x0F {
        return Err(format!("no lui at {hi:#010x} ({h:08x}): not the object the adapter is validated for"));
    }
    Ok((h << 16).wrapping_add(l as u16 as i16 as i32 as u32))
}

/// The target of the `jal` at `at`.
fn jal(m: &M, at: u32) -> std::result::Result<u32, String> {
    let w = m.mem.r32(at);
    if w >> 26 != 3 {
        return Err(format!("no jal at {at:#010x} ({w:08x}): not the object the adapter is validated for"));
    }
    Ok((at & 0xF000_0000) | (w & 0x03FF_FFFF) << 2)
}

/// Where `bios_1` keeps its state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Bios1 {
    pub text: u32,
    pub data: u32,
    pub bss: u32,
}

impl Bios1 {
    /// From the address of its function at `off`: `CD_initintr` stores to
    /// `CD_cbready` (`.data+0x0C`), the handler passes `.bss+8`.
    pub fn find(m: &M, entry: u32, off: u32) -> std::result::Result<Bios1, String> {
        let text = entry.wrapping_sub(off);
        let data = hi_lo(m, text + 0x110C, text + 0x1110)?.wrapping_sub(d::CBREADY);
        let bss = hi_lo(m, text + 0x150C, text + 0x1514)?.wrapping_sub(bs::READY);
        Ok(Bios1 { text, data, bss })
    }

    fn intr(self, m: &M, i: u32) -> u8 {
        m.mem.r8(self.data + d::INTR + i)
    }

    fn set_intr(self, m: &mut M, i: u32, v: u8) {
        m.mem.w8(self.data + d::INTR + i, v)
    }

    fn word(self, m: &M, off: u32) -> u32 {
        m.mem.r32(self.data + off)
    }

    /// A per-command table entry.
    fn table(self, m: &M, table: u32, com: u8) -> u32 {
        m.mem.r32(self.data + table + 4 * com as u32)
    }
}

/// The adapter for `bios_1`'s function at `off` just got called.
fn bios1(m: &mut M, off: u32) -> std::result::Result<Bios1, String> {
    let b = Bios1::find(m, m.sdk.at, off)?;
    m.sdk.cd.bios1 = Some(b);
    Ok(b)
}

/// Copy a result buffer to `to` (8 bytes), if there is one.
fn copy8(m: &mut M, from: u32, to: u32) {
    if to != 0 {
        let v = m.mem.bytes(from, 8);
        m.mem.write(to, &v);
    }
}

/// A call into the controller's core at the current time, the controller
/// brought up to now around it (its due work, its line, its wakeup).
fn cdc<R>(m: &mut M, f: impl FnOnce(&mut Cdc, u64) -> R) -> std::result::Result<R, String> {
    m.cdc_sync();
    let now = m.clock.now;
    let io = m.io.as_mut().ok_or("no CD controller on the bus")?;
    let r = f(&mut io.cdc, now);
    m.cdc_sync();
    Ok(r)
}

type Then = Box<dyn FnOnce(&mut M, u32) -> Flow<Psyq>>;
type Next = Box<dyn FnOnce(&mut M) -> Flow<Psyq>>;

fn call(addr: u32, args: [u32; 4], then: impl FnOnce(&mut M, u64) -> Flow<Psyq> + 'static) -> Flow<Psyq> {
    Flow::Call { call: GuestCall { addr, args }, then: Box::new(then) }
}

// --- The interrupt ---------------------------------------------------------

/// `bios_1`'s interrupt routine: the interrupt the controller presents, its
/// response, acknowledged; the drive status from it; the interrupt states
/// and result buffers. What to call: bit 1 the sync callback, bit 2 the
/// ready callback (0: nothing presented).
fn getintr(m: &mut M, b: Bios1) -> std::result::Result<u32, String> {
    let intr = cdc(m, |c, _| c.pending())? & 7;
    if intr == 0 {
        return Ok(0);
    }
    let resp = cdc(m, |c, _| {
        let r = c.take_response(8);
        c.acknowledge();
        c.set_enable(7);
        r
    })?;
    let mut buf = [0u8; 8];
    buf[..resp.len()].copy_from_slice(&resp);
    let com = m.mem.r8(b.data + d::COM);
    let mut stat = 0;
    if intr != 3 || b.table(m, d::ACK_STATUS, com) != 0 {
        if b.word(m, d::STATUS) & 0x10 == 0 && buf[0] & 0x10 != 0 {
            let n = b.word(m, d::NOPEN);
            m.mem.w32(b.data + d::NOPEN, n.wrapping_add(1));
        }
        stat = buf[0] & 0x1D;
        m.mem.w32(b.data + d::STATUS, buf[0] as u32);
        m.mem.w32(b.data + d::STATUS1, buf[1] as u32);
    }
    let put = |m: &mut M, at: u32| m.mem.write(b.bss + at, &buf);
    Ok(match intr {
        3 => {
            let (s, r) = if stat != 0 {
                (5, 2)
            } else if b.table(m, d::SECOND, com) != 0 {
                (3, 1)
            } else {
                (2, 2)
            };
            b.set_intr(m, 0, s);
            put(m, bs::SYNC);
            r
        }
        2 => {
            b.set_intr(m, 0, if stat != 0 { 5 } else { 2 });
            put(m, bs::SYNC);
            2
        }
        1 => {
            // A lone status byte with an error bit is no error.
            let err = stat != 0 && resp.len() != 1;
            b.set_intr(m, 1, if err { 5 } else { 1 });
            put(m, bs::READY);
            cdc(m, |c, _| c.request(false))?;
            4
        }
        4 => {
            b.set_intr(m, 2, 4);
            b.set_intr(m, 1, 4);
            put(m, bs::DATA);
            put(m, bs::READY);
            4
        }
        5 => {
            b.set_intr(m, 1, 5);
            b.set_intr(m, 0, 5);
            put(m, bs::SYNC);
            put(m, bs::READY);
            6
        }
        other => {
            m.log(&format!("CDROM: unknown intr ({other})"));
            0
        }
    })
}

/// The callbacks for `getintr`'s answer `f`: the ready one, then the sync
/// one, each with its interrupt state and result buffer.
fn callbacks(m: &M, b: Bios1, f: u32) -> (Option<GuestCall>, Option<GuestCall>) {
    let cb = |slot: u32, state: u32, buf: u32| {
        let addr = b.word(m, slot);
        (addr != 0).then(|| GuestCall { addr, args: [b.intr(m, state) as u32, b.bss + buf, 0, 0] })
    };
    let ready = if f & 4 != 0 { cb(d::CBREADY, 1, bs::READY) } else { None };
    let sync = if f & 2 != 0 { cb(d::CBSYNC, 0, bs::SYNC) } else { None };
    (ready, sync)
}

/// The line-2 handler (`bios_1`'s, installed by `CD_init`): while the
/// controller presents an interrupt, take it and call its callbacks.
/// State: 0 at the top of the loop, 1 after a callback, 0x101 after the
/// ready callback with the sync one still due.
pub fn stand_in(m: &mut M, addr: u32, state: u32) -> Option<StandIn> {
    if addr != HANDLER {
        return None;
    }
    let Some(b) = m.sdk.cd.bios1 else {
        m.log("libcd: interrupt handler without CD_init");
        return Some(StandIn::Return(0));
    };
    if state & 0x100 != 0 {
        if let (_, Some(c)) = callbacks(m, b, 2) {
            return Some(StandIn::Call(c, 1));
        }
    }
    loop {
        let f = getintr(m, b).unwrap_or_else(|e| {
            m.log(&format!("libcd interrupt: {e}"));
            0
        });
        if f == 0 {
            return Some(StandIn::Return(0));
        }
        match callbacks(m, b, f) {
            (Some(r), _) => return Some(StandIn::Call(r, 1 | (f & 2) << 7)),
            (None, Some(s)) => return Some(StandIn::Call(s, 1)),
            (None, None) => {}
        }
    }
}

/// The same from inside a wait in a callback (`CheckCallback()`): every
/// interrupt presented now, with its callbacks, then `then`.
fn poll(m: &mut M, b: Bios1, then: Next) -> Flow<Psyq> {
    let f = getintr(m, b).unwrap_or_else(|e| {
        m.log(&format!("libcd: {e}"));
        0
    });
    if f == 0 {
        return then(m);
    }
    let (ready, _) = callbacks(m, b, f);
    let sync_then = move |m: &mut M| -> Flow<Psyq> {
        match callbacks(m, b, f & 2).1 {
            Some(c) => Flow::Call { call: c, then: Box::new(move |m, _| poll(m, b, then)) },
            None => poll(m, b, then),
        }
    };
    match ready {
        Some(c) => Flow::Call { call: c, then: Box::new(move |m, _| sync_then(m)) },
        None => sync_then(m),
    }
}

// --- Waits ------------------------------------------------------------------

type Done = fn(&mut M, Bios1) -> Option<u32>;

/// One of the drive layer's wait loops (`CD_sync`, `CD_ready`, `CD_cw`):
/// until `done` gives the result, or with `once` after one look (0). A
/// thousand passes per safepoint quantum, interrupts taken between quanta;
/// inside a callback each quantum polls the controller instead. After
/// [`TIMEOUT_PASSES`]: "CD timeout", `CD_flush`, -1. A wait that spun
/// leaves the guest's safepoint budget as the loop's last safepoint did:
/// full (D60).
fn wait(m: &mut M, b: Bios1, what: &'static str, once: bool, done: Done, then: Then) -> Flow<Psyq> {
    let deadline = m.clock.now + (TIMEOUT_PASSES + 1) * quantum(m) / LOOP_BUDGET;
    pass(m, b, what, once, deadline, false, done, then)
}

#[allow(clippy::too_many_arguments)]
fn pass(m: &mut M, b: Bios1, what: &'static str, once: bool, deadline: u64, spun: bool, done: Done, then: Then) -> Flow<Psyq> {
    let end = move |m: &mut M, v: u32, then: Then| {
        if spun {
            m.regs.reset_budget();
        }
        then(m, v)
    };
    if m.clock.now >= deadline {
        let com = m.mem.r8(b.data + d::COM);
        m.log(&format!("CD timeout: {what}: com={com} sync={} ready={}", b.intr(m, 0), b.intr(m, 1)));
        flush(m, b);
        return end(m, u32::MAX, then);
    }
    let rest: Next = Box::new(move |m: &mut M| {
        if let Some(v) = done(m, b) {
            return end(m, v, then);
        }
        if once {
            return end(m, 0, then);
        }
        let until = m.clock.now + quantum(m);
        m.run_until(until);
        Flow::Block { until: Wait::Cycle(until), then: Box::new(move |m| pass(m, b, what, once, deadline, true, done, then)) }
    });
    // In a callback (libetc's dispatcher, or any exception) no interrupt
    // is taken: the loop polls the controller itself.
    if m.in_irq { poll(m, b, rest) } else { rest(m) }
}

/// `CD_sync(mode, result)`: the command's completion (2) or error (5), its
/// result copied, the state back to complete. Without waiting: 0 if
/// neither.
fn sync(m: &mut M, b: Bios1, once: bool, result: u32, then: Then) -> Flow<Psyq> {
    let done: Done = |m, b| {
        let s = b.intr(m, 0);
        (s == 2 || s == 5).then_some(s as u32)
    };
    wait(
        m,
        b,
        "CD_sync",
        once,
        done,
        Box::new(move |m, v| {
            if v == 2 || v == 5 {
                b.set_intr(m, 0, 2);
                copy8(m, b.bss + bs::SYNC, result);
            }
            then(m, v)
        }),
    )
}

/// `CD_ready(mode, result)`: a data-end interrupt (4), else data ready (1)
/// or error (5), taken with its result. Without waiting: 0 if none.
fn ready(m: &mut M, b: Bios1, once: bool, result: u32, then: Then) -> Flow<Psyq> {
    let done: Done = |m, b| {
        let (end, data) = (b.intr(m, 2), b.intr(m, 1));
        if end != 0 {
            Some(0x200 | end as u32)
        } else if data != 0 {
            Some(0x100 | data as u32)
        } else {
            None
        }
    };
    wait(
        m,
        b,
        "CD_ready",
        once,
        done,
        Box::new(move |m, v| {
            if v == 0 || v == u32::MAX {
                return then(m, v);
            }
            let (slot, from) = if v >> 8 == 2 { (2, bs::DATA) } else { (1, bs::READY) };
            b.set_intr(m, slot, 0);
            copy8(m, b.bss + from, result);
            then(m, v & 0xFF)
        }),
    )
}

/// `CD_cw(com, param, result, async)`: after the last command completes,
/// the command to the controller (Setloc and Setmode remembered); unless
/// `async`, its first answer, its result copied: 0, or -1 on an error
/// answer; -2 without a parameter it needs.
fn cw(m: &mut M, b: Bios1, com: u8, param: u32, result: u32, nowait: bool, then: Then) -> Flow<Psyq> {
    let n = b.table(m, d::NPARAMS, com);
    if n != 0 && param == 0 {
        return then(m, (-2i32) as u32);
    }
    let issue = move |m: &mut M, _| {
        if com == com::SETLOC {
            let pos = m.mem.bytes(param, 4);
            m.mem.write(b.data + d::POS, &pos);
        }
        if com == com::SETMODE {
            let mode = m.mem.r8(param);
            m.mem.w8(b.data + d::MODE, mode);
        }
        b.set_intr(m, 0, 0);
        if b.table(m, d::READY_RESET, com) != 0 {
            b.set_intr(m, 1, 0);
        }
        let params = if n > 0 { m.mem.bytes(param, n.min(16) as usize) } else { Vec::new() };
        m.mem.w8(b.data + d::COM, com);
        if let Err(e) = cdc(m, |c, now| c.issue(com, &params, now)) {
            m.log(&format!("CD_cw: {e}"));
        }
        if nowait {
            return then(m, 0);
        }
        let answered: Done = |m, b| (b.intr(m, 0) != 0).then_some(0);
        let answer: Then = Box::new(move |m, v| {
            if v == u32::MAX {
                return then(m, v);
            }
            copy8(m, b.bss + bs::SYNC, result);
            then(m, if b.intr(m, 0) == 5 { u32::MAX } else { 0 })
        });
        if b.intr(m, 0) != 0 { answer(m, 0) } else { wait(m, b, "CD_cw", false, answered, answer) }
    };
    sync(m, b, false, 0, Box::new(issue))
}

/// `CD_flush`: acknowledge everything pending, the states back to idle,
/// the data FIFO emptied. Its result is a register value left in v0.
fn flush(m: &mut M, b: Bios1) -> u32 {
    let r = cdc(m, |c, _| {
        while c.pending() & 7 != 0 {
            c.acknowledge();
            c.set_enable(7);
        }
        c.request(false);
    });
    if let Err(e) = r {
        m.log(&format!("CD_flush: {e}"));
    }
    b.set_intr(m, 2, 0);
    b.set_intr(m, 1, 0);
    b.set_intr(m, 0, 2);
    0x1325
}

fn value(v: u32) -> Flow<Psyq> {
    Flow::ret(v)
}

// --- bios_1 -----------------------------------------------------------------

fn cd_cw(m: &mut M, a: [u32; 4]) -> Result {
    let b = bios1(m, at::CD_CW)?;
    Ok(cw(m, b, a[0] as u8, a[1], a[2], a[3] != 0, Box::new(|_, v| value(v))))
}

fn cd_sync(m: &mut M, a: [u32; 4]) -> Result {
    let b = bios1(m, at::CD_SYNC)?;
    Ok(sync(m, b, a[0] != 0, a[1], Box::new(|_, v| value(v))))
}

fn cd_ready(m: &mut M, a: [u32; 4]) -> Result {
    let b = bios1(m, at::CD_READY)?;
    Ok(ready(m, b, a[0] != 0, a[1], Box::new(|_, v| value(v))))
}

fn cd_flush(m: &mut M, _a: [u32; 4]) -> Result {
    let b = bios1(m, at::CD_FLUSH)?;
    ret(flush(m, b))
}

/// `CD_vol(vol)`: the CD-to-SPU volume matrix, applied.
fn cd_vol(m: &mut M, a: [u32; 4]) -> Result {
    bios1(m, at::CD_VOL)?;
    let v = m.mem.bytes(a[0], 4);
    cdc(m, |c, _| c.set_volume([v[0], v[1], v[2], v[3]]))?;
    ret(0)
}

/// `CD_initvol`: the SPU's main volume if it is silent, its CD input
/// volume and SPUCNT (enabled, unmuted, CD audio on), and the CD's matrix
/// straight through. The SPU's registers are its device's (shared, as the
/// interrupt controller's are for chip irq) until chip spu has adapters.
fn cd_initvol(m: &mut M, _a: [u32; 4]) -> Result {
    bios1(m, at::CD_INITVOL)?;
    if reg(m, SPU + 0x1B8, 2)? == 0 && reg(m, SPU + 0x1BA, 2)? == 0 {
        set_reg(m, SPU + 0x180, 2, 0x3FFF)?;
        set_reg(m, SPU + 0x182, 2, 0x3FFF)?;
    }
    set_reg(m, SPU + 0x1B0, 2, 0x3FFF)?;
    set_reg(m, SPU + 0x1B2, 2, 0x3FFF)?;
    set_reg(m, SPU + 0x1AA, 2, 0xC001)?;
    cdc(m, |c, _| c.set_volume([0x80, 0, 0x80, 0]))?;
    ret(0)
}

/// The callback slots and status cleared, then `ResetCallback()` and
/// `InterruptCallback(2, handler)` as guest calls (the `jal`s at `reset`
/// and `intr` in bios_1's text), so the handler goes in whichever way
/// libetc runs.
fn install(m: &mut M, b: Bios1, reset: u32, intr: u32, then: impl FnOnce(&mut M, u64) -> Flow<Psyq> + 'static) -> Result {
    for o in [d::CBREADY, d::CBSYNC, d::STATUS1, d::STATUS] {
        m.mem.w32(b.data + o, 0);
    }
    let (reset, intr) = (jal(m, b.text + reset)?, jal(m, b.text + intr)?);
    Ok(call(reset, [0; 4], move |_, _| call(intr, [2, HANDLER, 0, 0], then)))
}

/// `CD_initintr`: the handler installed; `InterruptCallback`'s result.
fn cd_initintr(m: &mut M, _a: [u32; 4]) -> Result {
    let b = bios1(m, at::CD_INITINTR)?;
    install(m, b, 0x1128, 0x1138, |_, v| Flow::Return(v & 0xFFFF_FFFF))
}

/// `CD_init`: no command, mode 0, the handler installed, whatever the
/// controller presents acknowledged, the states idle, then Nop (twice
/// with the shell open), Init and Demute, and Init's completion: 0, or -1.
fn cd_init(m: &mut M, _a: [u32; 4]) -> Result {
    let b = bios1(m, at::CD_INIT)?;
    m.log(&format!("CD_init:addr={:08x}", b.data + d::INIT_ADDR));
    m.mem.w8(b.data + d::COM, 0);
    m.mem.w8(b.data + d::MODE, 0);
    let fail = (-1i32) as u32;
    let demute: Then = Box::new(move |m, v| {
        if v != 0 {
            return value(fail);
        }
        sync(m, b, false, 0, Box::new(move |_, v| value(if v == 2 { 0 } else { fail })))
    });
    let init: Then = Box::new(move |m, v| if v != 0 { value(fail) } else { cw(m, b, com::DEMUTE, 0, 0, false, demute) });
    let after_nop: Then = Box::new(move |m, _| {
        let start = move |m: &mut M| cw(m, b, com::INIT, 0, 0, false, init);
        if b.word(m, d::STATUS) & 0x10 != 0 { cw(m, b, com::NOP, 0, 0, false, Box::new(move |m, _| start(m))) } else { start(m) }
    });
    install(m, b, 0x11A8, 0x11B8, move |m, _| {
        flush(m, b);
        cw(m, b, com::NOP, 0, 0, false, after_nop)
    })
}

/// `CD_datasync(mode)`: 0 once DMA3 is idle; with `mode`, 1 while it is
/// busy. Transfers end at once here, so it never waits.
fn cd_datasync(m: &mut M, a: [u32; 4]) -> Result {
    bios1(m, at::CD_DATASYNC)?;
    let busy = reg(m, CHCR3, 4)? & CHCR_BUSY != 0;
    ret((busy && a[0] != 0) as u32)
}

// --- bios_2, bios_3 ---------------------------------------------------------

/// DMA3 from the controller's data FIFO: the sector buffer into the FIFO,
/// channel 3 enabled, then the transfer of `words` words to `madr` with
/// `chcr`, through the DMA controller's registers (shared with the other
/// chips: completion flags DICR and raises IRQ3 when enabled, as on the
/// device).
fn getsector(m: &mut M, madr: u32, words: u32, chcr: u32) -> Result {
    cdc(m, |c, _| c.request(true))?;
    let dpcr = reg(m, DPCR, 4)?;
    set_reg(m, DPCR, 4, dpcr | 0x8000)?;
    set_reg(m, MADR3, 4, madr)?;
    set_reg(m, BCR3, 4, words | 0x1_0000)?;
    set_reg(m, CHCR3, 4, chcr)?;
    m.cdc_sync();
    ret(0)
}

fn cd_getsector(m: &mut M, a: [u32; 4]) -> Result {
    getsector(m, a[0], a[1], 0x1100_0000)
}

fn cd_getsector2(m: &mut M, a: [u32; 4]) -> Result {
    getsector(m, a[0], a[1], 0x1140_0100)
}

// --- The streaming ring (c_011, c_003) -------------------------------------

/// The streaming ring's globals, as `StCdInterrupt`'s own code addresses
/// them (offsets of its relocated instruction pairs in `c_011`).
#[derive(Debug, Clone, Copy)]
struct Ring {
    final_sector: u32,
    rgb24: u32,
    emu_addr: u32,
    emu_idx: u32,
    intr_flag: u32,
    /// `c_011`'s `.data`: its register pointers, and a status word at 0x48.
    data: u32,
    /// `c_011`'s `.bss`: the ring entry being filled.
    entry: u32,
    idx1: u32,
    idx2: u32,
    addr: u32,
    size: u32,
    base: u32,
    mode: u32,
    start_flag: u32,
    start_frame: u32,
    end_frame: u32,
    sector_offset: u32,
    frame_no: u32,
    cchannel: u32,
    stchannel: u32,
    func2: u32,
    cd_ready: u32,
    init_ring_status: u32,
    data_ready_callback: u32,
}

impl Ring {
    fn find(m: &M, t: u32) -> std::result::Result<Ring, String> {
        let p = |hi: u32, lo: u32| hi_lo(m, t + hi, t + lo);
        Ok(Ring {
            final_sector: p(0x04, 0x08)?,
            rgb24: p(0x18, 0x1C)?,
            data: p(0x2C, 0x30)?.wrapping_sub(0x20),
            emu_addr: p(0x4C, 0x50)?,
            intr_flag: p(0x54, 0x5C)?,
            emu_idx: p(0x60, 0x64)?,
            idx1: p(0xC8, 0xCC)?,
            addr: p(0xD0, 0xD4)?,
            entry: p(0xE0, 0xE4)?,
            mode: p(0x194, 0x198)?,
            start_flag: p(0x2D0, 0x2D4)?,
            start_frame: p(0x2E4, 0x2E8)?,
            cchannel: p(0x370, 0x374)?,
            sector_offset: p(0x3C8, 0x3CC)?,
            frame_no: p(0x3E0, 0x3E4)?,
            idx2: p(0x404, 0x408)?,
            end_frame: p(0x4A4, 0x4A8)?,
            func2: p(0x514, 0x518)?,
            size: p(0x570, 0x574)?,
            base: p(0x724, 0x728)?,
            stchannel: p(0x814, 0x818)?,
            cd_ready: jal(m, t + 0x84)?,
            init_ring_status: jal(m, t + 0x424)?,
            data_ready_callback: jal(m, t + 0x904)?,
        })
    }

    fn status(self, m: &mut M, code: u32) {
        m.mem.w32(self.data + 0x48, code);
    }

    /// Streaming from memory (`StSetEmulate`): the next sector.
    fn emu_step(self, m: &mut M) {
        if m.mem.r32(self.emu_addr) != 0 {
            let i = m.mem.r32(self.emu_idx);
            m.mem.w32(self.emu_idx, i.wrapping_add(1));
        }
    }
}

/// `StCdInterrupt`'s DMA helper: channel 3's interrupt enabled for a
/// frame's last sector and disabled otherwise (a byte store to DICR, as
/// the original makes it), the channel enabled, then the transfer.
fn ring_dma(m: &mut M, madr: u32, words: u32, chcr: u32, irq: bool) -> std::result::Result<(), String> {
    let enables = reg(m, DICR + 2, 1)?;
    set_reg(m, DICR + 2, 1, if irq { enables | 1 << CH } else { enables & !(1 << CH) })?;
    let dpcr = reg(m, DPCR, 4)?;
    set_reg(m, DPCR, 4, dpcr | 1 << (CH * 4 + 3))?;
    set_reg(m, MADR3, 4, madr)?;
    set_reg(m, BCR3, 4, words)?;
    set_reg(m, CHCR3, 4, chcr)?;
    m.cdc_sync();
    Ok(())
}

fn copy_words(m: &mut M, to: u32, from: u32, n: u32) {
    let v = m.mem.bytes(from, 4 * n as usize);
    m.mem.write(to, &v);
}

/// `StCdInterrupt()`: the data-ready interrupt's work for the streaming
/// ring (libds's and the game's ready callbacks call it). It takes the
/// controller's data interrupt (`CdReady(1, …)`), reads the sector's
/// header bytes, DMAs its 32-byte frame header into the ring entry at
/// `StRingIdx1` and its 2016 data bytes into the entry's slot, keeps the
/// frame and sector counters, starts over on a frame out of sequence, ends
/// at `StEndFrame`, wraps when the frame won't fit (`StFunc2`), and for a
/// frame's last sector enables DMA3's interrupt, whose callback
/// (`data_ready_callback`) hands the frame on. The guest calls it makes are
/// the original's (`CdReady`, `init_ring_status`, `StFunc2`), from a frame
/// below the caller's stack that holds `CdReady`'s result.
fn st_cd_interrupt(m: &mut M, _a: [u32; 4]) -> Result {
    let s = Ring::find(m, m.sdk.at)?;
    if m.mem.r32(s.final_sector) == 1 {
        return ret(0);
    }
    if m.mem.r32(s.rgb24) != 0 {
        // 24-bit movies skip sectors while the GPU's DMA (the last frame)
        // runs.
        let gpu = m.mem.r32(s.data + 0x20);
        if reg(m, gpu, 4)? & CHCR_BUSY != 0 {
            m.mem.w32(s.intr_flag, 1);
            s.emu_step(m);
            s.status(m, 1);
            return ret(0);
        }
    }
    let frame = m.regs.sp().wrapping_sub(64);
    m.regs.set_sp(frame);
    Ok(call(s.cd_ready, [1, frame + 48, 0, 0], move |m, v| match ring_sector(m, s, frame, v as u32) {
        Ok(f) => f(m),
        Err(e) => {
            m.log(&format!("StCdInterrupt: {e}"));
            leave(m, frame)
        }
    }))
}

/// The end of `StCdInterrupt`: its stack frame popped.
fn leave(m: &mut M, frame: u32) -> Flow<Psyq> {
    m.regs.set_sp(frame.wrapping_add(64));
    Flow::ret(0)
}

/// `StCdInterrupt` after `CdReady`: what remains, as a flow to run.
fn ring_sector(m: &mut M, s: Ring, frame: u32, ready: u32) -> std::result::Result<Next, String> {
    let done = move |code: Option<u32>| -> Next {
        Box::new(move |m: &mut M| {
            if let Some(c) = code {
                s.status(m, c);
            }
            leave(m, frame)
        })
    };
    if ready == 5 {
        return Ok(done(None));
    }
    if m.mem.r8(frame + 48) & 4 != 0 {
        return Ok(done(Some(3)));
    }
    let e = m.mem.r32(s.addr).wrapping_add(m.mem.r32(s.idx1).wrapping_mul(32));
    m.mem.w32(s.entry, e);
    if m.mem.r16(e) != 0 {
        // The ring is full.
        s.emu_step(m);
        return Ok(done(Some(4)));
    }
    cdc(m, |c, _| {
        c.request(false);
        c.request(true);
    })?;
    let hdr = frame + 40;
    if m.mem.r32(s.mode) == 0 {
        let h = cdc(m, |c, _| {
            let h = c.read_data(4);
            c.read_data(8);
            h
        })?;
        m.mem.write(hdr, &h);
    }
    let emu = m.mem.r32(s.emu_addr);
    if emu != 0 {
        let from = emu.wrapping_add(m.mem.r32(s.emu_idx).wrapping_mul(2048));
        copy_words(m, e, from, 8);
    } else {
        ring_dma(m, e, 8, 0x1100_0000, false)?;
    }
    let h = m.mem.bytes(hdr, 4);
    m.mem.write(e + 0x1C, &h);
    let start = m.mem.r32(s.start_frame);
    if m.mem.r32(s.start_flag) == 1 && start != 0 {
        if start != m.mem.r16(e + 8) as u32 {
            m.mem.w16(e, 0);
            s.emu_step(m);
            return Ok(done(None));
        }
        m.mem.w32(s.start_flag, 0);
    }
    let e = m.mem.r32(s.entry);
    if m.mem.r16(e) != 0x160 || (m.mem.r16(e + 2) >> 10 & 0x1F) as u32 != m.mem.r32(s.cchannel) {
        if m.mem.r32(s.emu_addr) != 0 {
            m.mem.w32(s.emu_idx, 0);
        }
        m.mem.w16(e, 0);
        return Ok(done(Some(5)));
    }
    let offset = m.mem.r16(s.sector_offset) as i16 as i32 as u32;
    let frame_no = m.mem.r32(s.frame_no);
    // Starting over: the ring entries from StRingIdx2 cleared.
    let restart = move |m: &mut M, then: Next| -> Flow<Psyq> {
        let (i1, i2) = (m.mem.r32(s.idx1), m.mem.r32(s.idx2));
        m.mem.w32(s.frame_no, 0);
        m.mem.w16(s.sector_offset, 0);
        call(s.init_ring_status, [i2, i1.wrapping_sub(i2), 0, 0], move |m, _| {
            let i2 = m.mem.r32(s.idx2);
            m.mem.w32(s.idx1, i2);
            let e = m.mem.r32(s.entry);
            m.mem.w16(e, 0);
            then(m)
        })
    };
    let step_and = move |code: u32| -> Next {
        Box::new(move |m: &mut M| {
            s.emu_step(m);
            done(Some(code))(m)
        })
    };
    if offset != m.mem.r16(e + 4) as u32 || (frame_no != 0 && frame_no != m.mem.r16(e + 8) as u32) {
        // Out of sequence.
        return Ok(Box::new(move |m: &mut M| restart(m, step_and(6))));
    }
    let func2 = move |m: &mut M, then: Next| -> Flow<Psyq> {
        let f = m.mem.r32(s.func2);
        if f != 0 { call(f, [0; 4], move |m, _| then(m)) } else { then(m) }
    };
    if m.mem.r16(e + 4) == 0 {
        // A frame's first sector.
        m.mem.w16(s.sector_offset, 0);
        let no = m.mem.r16(e + 8) as u32;
        m.mem.w32(s.frame_no, no);
        let end = m.mem.r32(s.end_frame);
        if end != 0 && no >= end {
            // Past the last frame: the stream ends.
            return Ok(Box::new(move |m: &mut M| {
                restart(
                    m,
                    Box::new(move |m: &mut M| {
                        m.mem.w32(s.start_flag, 1);
                        func2(m, step_and(7))
                    }),
                )
            }));
        }
        let room = m.mem.r32(s.size).wrapping_sub(m.mem.r32(s.idx1)).wrapping_sub(1);
        if room < m.mem.r16(e + 6) as u32 {
            // The frame doesn't fit before the ring's end.
            if m.mem.r32(s.end_frame) == 0 {
                m.mem.w16(e, 1);
                m.mem.w32(s.start_flag, 1);
                return Ok(Box::new(move |m: &mut M| func2(m, step_and(8))));
            }
            let ring = m.mem.r32(s.addr);
            if m.mem.r16(ring) != 0 {
                m.mem.w16(e, 0);
                s.emu_step(m);
                return Ok(done(Some(9)));
            }
            // Wrap: the entry moves to the ring's start.
            m.mem.w16(e, 1);
            m.mem.w32(s.idx1, 0);
            copy_words(m, ring, e, 8);
            m.mem.w32(s.entry, ring);
        }
        let i1 = m.mem.r32(s.idx1);
        m.mem.w32(s.idx2, i1);
    }
    s.status(m, 10);
    let so = m.mem.r16(s.sector_offset);
    m.mem.w16(s.sector_offset, so.wrapping_add(1));
    let base = m.mem.r32(s.addr).wrapping_add(m.mem.r32(s.size).wrapping_mul(32)).wrapping_add(m.mem.r32(s.idx1).wrapping_mul(2016));
    m.mem.w32(s.base, base);
    let chcr = if m.mem.r32(s.rgb24) != 0 { 0x1100_0000 } else { 0x1140_0100 };
    let e = m.mem.r32(s.entry);
    let last = m.mem.r16(e + 6) as i32 - 1 == m.mem.r16(e + 4) as i32;
    if last {
        m.mem.w32(s.final_sector, 1);
    }
    let emu = m.mem.r32(s.emu_addr);
    if emu != 0 {
        let i = m.mem.r32(s.emu_idx);
        copy_words(m, base, emu.wrapping_add(i.wrapping_mul(2048)).wrapping_add(32), 504);
        m.mem.w32(s.emu_idx, i.wrapping_add(1));
    } else {
        ring_dma(m, base, 504, chcr, last)?;
    }
    if last {
        m.mem.w16(s.sector_offset, 0);
        m.mem.w32(s.frame_no, 0);
        let ch = m.mem.r32(s.stchannel);
        m.mem.w32(s.cchannel, ch);
    }
    let e = m.mem.r32(s.entry);
    m.mem.w16(e, 3);
    let i1 = m.mem.r32(s.idx1);
    m.mem.w32(s.idx1, i1.wrapping_add(1));
    if m.mem.r32(s.emu_addr) != 0 && m.mem.r32(s.final_sector) != 0 {
        // From memory no DMA interrupt comes: the frame is handed on here.
        return Ok(Box::new(move |_: &mut M| call(s.data_ready_callback, [0; 4], move |m, _| leave(m, frame))));
    }
    Ok(done(None))
}

/// `StUnSetRing()`: in a critical section, the data and ready callbacks
/// off (libds's when it is active, else libcd's), and the data FIFO
/// emptied. The calls are the original's; `ExitCriticalSection`'s result.
fn st_unset_ring(m: &mut M, _a: [u32; 4]) -> Result {
    let t = m.sdk.at;
    let ds_active = hi_lo(m, t + 0x10, t + 0x14)?;
    let enter = jal(m, t + 0x08)?;
    let (data, ready) = if m.mem.r32(ds_active) == 1 { (jal(m, t + 0x24)?, jal(m, t + 0x2C)?) } else { (jal(m, t + 0x3C)?, jal(m, t + 0x44)?) };
    let exit = jal(m, t + 0x68)?;
    Ok(call(enter, [0; 4], move |_, _| {
        call(data, [0; 4], move |_, _| {
            call(ready, [0; 4], move |m, _| {
                if let Err(e) = cdc(m, |c, _| c.request(false)) {
                    m.log(&format!("StUnSetRing: {e}"));
                }
                call(exit, [0; 4], |_, v| Flow::Return(v & 0xFFFF_FFFF))
            })
        })
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{call as run, irq_calls, machine};
    use psx_devices::cd;

    /// `lui reg, hi(v)` and a low-16 instruction with `lo(v)`.
    fn pair(m: &mut M, at_hi: u32, at_lo: u32, v: u32) {
        let lo = v as u16;
        let hi = v.wrapping_sub(lo as i16 as i32 as u32) >> 16;
        m.mem.w32(at_hi, 0x3C02_0000 | hi);
        m.mem.w32(at_lo, 0x8C42_0000 | lo as u32);
    }

    fn jal_to(m: &mut M, at: u32, target: u32) {
        m.mem.w32(at, 0x0C00_0000 | (target & 0x0FFF_FFFF) >> 2);
    }

    const TEXT: u32 = 0x8003_0000;
    const DATA: u32 = 0x8005_8000;
    const BSS: u32 = 0x8005_F000;
    const RESET: u32 = 0x8004_0000;
    const INTR: u32 = 0x8004_0100;

    fn disc() -> Vec<[u8; cd::RAW]> {
        (0..40u32)
            .map(|lba| {
                let mut s = [0u8; cd::RAW];
                s[..12].copy_from_slice(&[0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0]);
                s[12..15].copy_from_slice(&cd::lba_to_msf(lba));
                s[15] = 2;
                s[0x18..0x818].fill(lba as u8);
                let e = cd::edc(&s[0x10..0x818]);
                s[0x818..0x81C].copy_from_slice(&e.to_le_bytes());
                s
            })
            .collect()
    }

    fn adapter(name: &str) -> Adapter {
        crate::adapters().into_iter().find(|(n, _)| *n == name).map(|(_, f)| f).unwrap()
    }

    /// A machine with a bus and a disc, bios_1 at TEXT with PsyQ 4.7's
    /// tables for the commands used here, libetc's dispatcher running.
    fn bios1_machine() -> M {
        let mut m = machine(disc(), Default::default());
        m.io = Some(Default::default());
        pair(&mut m, TEXT + 0x110C, TEXT + 0x1110, DATA + d::CBREADY);
        pair(&mut m, TEXT + 0x150C, TEXT + 0x1514, BSS + bs::READY);
        for (r, i) in [(RESET, 0x11A8), (INTR, 0x11B8), (RESET, 0x1128), (INTR, 0x1138)] {
            jal_to(&mut m, TEXT + i, r);
        }
        // Setloc and Setmode take parameters; Standby and Init complete
        // with INT2; ReadN clears the ready state.
        for (c, n) in [(2u32, 3u32), (14, 1), (13, 2)] {
            m.mem.w32(DATA + d::NPARAMS + 4 * c, n);
        }
        for c in [7u32, 8, 9, 10, 21, 22] {
            m.mem.w32(DATA + d::SECOND + 4 * c, 1);
        }
        for c in [6u32, 27] {
            m.mem.w32(DATA + d::READY_RESET + 4 * c, 1);
        }
        for c in 0..32u32 {
            m.mem.w32(DATA + d::ACK_STATUS + 4 * c, 1);
        }
        // The guest calls CD_init makes do nothing here: the handler goes
        // on line 2 as libetc's InterruptCallback would put it.
        run(&mut m, adapter("ResetCallback"), [0; 4]);
        run(&mut m, adapter("InterruptCallback"), [2, HANDLER, 0, 0]);
        m
    }

    fn at(m: &mut M, off: u32) {
        m.sdk.at = TEXT + off;
    }

    #[test]
    fn finds_its_state_through_its_own_code() {
        let m = bios1_machine();
        let b = Bios1::find(&m, TEXT + at::CD_CW, at::CD_CW).unwrap();
        assert_eq!(b, Bios1 { text: TEXT, data: DATA, bss: BSS });
        assert!(Bios1::find(&m, TEXT + 4 + at::CD_CW, at::CD_CW).is_err(), "not the validated layout");
    }

    #[test]
    fn init_and_commands() {
        let mut m = bios1_machine();
        at(&mut m, at::CD_INIT);
        assert_eq!(run(&mut m, cd_init, [0; 4]), 0);
        assert_eq!(irq_calls()[..2], [RESET, INTR], "ResetCallback, InterruptCallback");
        assert_eq!(m.mem.r8(DATA + d::COM), com::DEMUTE);
        assert_eq!(m.mem.r8(DATA + d::INTR), 2);
        // Setloc, then Setmode: kept where CdLastPos and CdMode read them.
        m.mem.write(0x8001_0000, &[0x00, 0x02, 0x16, 0x00]);
        at(&mut m, at::CD_CW);
        assert_eq!(run(&mut m, cd_cw, [com::SETLOC as u32, 0x8001_0000, 0x8001_0100, 0]), 0);
        assert_eq!(m.mem.bytes(DATA + d::POS, 4), [0x00, 0x02, 0x16, 0x00]);
        assert_eq!(m.mem.r8(0x8001_0100) & 0x02, 0x02, "the status byte: motor on");
        m.mem.w8(0x8001_0000, 0x80);
        assert_eq!(run(&mut m, cd_cw, [com::SETMODE as u32, 0x8001_0000, 0, 0]), 0);
        assert_eq!(m.mem.r8(DATA + d::MODE), 0x80);
        assert_eq!(run(&mut m, cd_cw, [com::SETMODE as u32, 0, 0, 0]) as i32, -2, "Setmode needs its parameter");
        // Standby: acknowledged, then complete; CD_sync waits for that.
        m.mem.w32(DATA + d::CBSYNC, 0x8002_2000);
        assert_eq!(run(&mut m, cd_cw, [7, 0, 0, 0]), 0);
        assert_eq!(m.mem.r8(DATA + d::INTR), 3);
        at(&mut m, at::CD_SYNC);
        assert_eq!(run(&mut m, cd_sync, [1, 0, 0, 0]), 0, "not yet");
        assert_eq!(run(&mut m, cd_sync, [0, 0x8001_0200, 0, 0]), 2);
        assert_ne!(m.mem.r8(0x8001_0200), 0);
        assert!(irq_calls().contains(&0x8002_2000), "the sync callback ran at INT2");
    }

    #[test]
    fn sectors_come_ready_and_by_dma() {
        let mut m = bios1_machine();
        at(&mut m, at::CD_INIT);
        assert_eq!(run(&mut m, cd_init, [0; 4]), 0);
        // Setmode (Init left 2340-byte sectors), Setloc 20, ReadN, with a
        // ready callback as CdReadyCallback sets it.
        at(&mut m, at::CD_CW);
        m.mem.w8(0x8001_0000, 0x80);
        run(&mut m, cd_cw, [com::SETMODE as u32, 0x8001_0000, 0, 0]);
        m.mem.write(0x8001_0000, &cd::lba_to_msf(20));
        run(&mut m, cd_cw, [com::SETLOC as u32, 0x8001_0000, 0, 0]);
        m.mem.w32(DATA + d::CBREADY, 0x8002_0000);
        run(&mut m, cd_cw, [6, 0, 0, 0]);
        irq_calls();
        at(&mut m, at::CD_READY);
        assert_eq!(run(&mut m, cd_ready, [0, 0x8001_0300, 0, 0]), 1);
        assert!(irq_calls().contains(&0x8002_0000), "the ready callback ran");
        // The sector by DMA3, its callback through libetc's DMA handler.
        run(&mut m, adapter("DMACallback"), [3, 0x8002_1000, 0, 0]);
        run(&mut m, cd_getsector, [0x8010_0000, 512, 0, 0]);
        assert_eq!(m.mem.r8(0x8010_0000), 20);
        assert_eq!(m.mem.r8(0x8010_07FF), 20);
        crate::testing::drain(&mut m);
        assert_eq!(irq_calls(), vec![0x8002_1000]);
        at(&mut m, at::CD_DATASYNC);
        assert_eq!(run(&mut m, cd_datasync, [1, 0, 0, 0]), 0);
        assert!(m.io.as_ref().unwrap().cdc.edc_errors.is_empty());
    }
}
