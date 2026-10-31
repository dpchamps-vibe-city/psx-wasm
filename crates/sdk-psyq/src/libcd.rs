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
//! the handler on line 2 is `bios_1`'s own, at its address: an adapter
//! bound by its place (`libcd/bios_1:0x14a4`, a static function) that
//! `CD_init` installs through the guest's own `InterruptCallback`, so
//! libetc dispatches it whichever way libetc runs. Sector transfers go
//! through DMA3 on the DMA controller the chips share, so DMA3's callback
//! (`CdDataCallback`, the ring's frame callback) arrives as on the device.
//! XA audio is the controller's (its decoder, into the SPU's CD input).
//!
//! **Time** (D71): every adapter spends its original's blocks on the path
//! it takes (offsets in its object, `psxw sdk disasm`), each before what
//! time can see, and the `VSync(-1)` and `CheckCallback()` the waits call
//! as libetc's adapters spend them. A loop header of the original's,
//! where the budget has run out, takes a safepoint ([`header`]): the
//! waits', the result copies', `CD_flush`'s, the ring's; the interrupt
//! handler and `getintr` run in the exception, a quantum passing at their
//! headers ([`crate::irq_header`]). Waits on what the models answer at once
//! (DMA3 idle, the FIFO holding the sector, the flags read twice) take
//! one pass.
//!
//! **Waits spin,** as the original's loops do: each pass its instructions
//! from the guest's budget, a safepoint each time it runs out, interrupts
//! taken there (D59's rule for adapters that replace a loop). Inside a
//! callback, where no interrupt is taken, a wait polls the controller and
//! calls the callbacks itself, as the original does when `CheckCallback()`
//! says so. Past 0x3C0000 passes a wait gives up as the original does
//! ("CD timeout", `CD_flush`, -1).
//!
//! **Not reproduced:** the debug printouts (`CD_debug`); the waits' other
//! time-out, 960 vblanks after the call (`VSync(-1)`), which comes a
//! little before the passes' in NTSC.

use crate::libetc::quantum;
use crate::{Adapter, M, Psyq, Result, reg, ret, set_reg};
use psx_bios::{Flow, GuestCall, Wait};
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
    // bios_1's interrupt handler, static: bound by its place.
    ("libcd/bios_1:0x14a4", handler),
];

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
    /// The interrupt handler `CD_init` and `CD_initintr` install (static).
    pub const HANDLER: u32 = 0x14a4;
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
    /// The names the timeout message gives: per command, per interrupt
    /// state (string pointers).
    pub const COM_NAMES: u32 = 0x2C;
    pub const INTR_NAMES: u32 = 0xAC;
}

/// One of `bios_1`'s wait loops, laid out alike from its header (D71):
/// `VSync(-1)`'s `jal` at `head`, the time-out checks to `count`, then the
/// time-out's blocks ("CD timeout: " through `puts`, whose `jal` is at
/// `count` + 8, the details through `printf`, `CD_flush`), the branch out
/// (`count` + 70h), and in the waits that poll the controller in a
/// callback, `CheckCallback()`'s `jal` (`count` + 78h) and that poll (its
/// loop's header at `count` + A0h, out at `count` + 118h), then the wait's
/// own look.
#[derive(Debug, Clone, Copy)]
struct Site {
    /// The `lui`/`addiu` of its name, for the time-out's message.
    name: u32,
    head: u32,
    count: u32,
    poll: bool,
    /// The loop's own instructions a pass, not timing out and not in a
    /// callback: `CD_sync` +5E4h..+7B0h, `CD_ready` +864h..+A78h, `CD_cw`
    /// +CC0h..+E3Ch, `CD_datasync` +1398h..+1474h.
    pass: u32,
    /// What it returns when it looks once and nothing came.
    not_yet: u32,
    /// Its epilogue.
    ret: (u32, u32),
}

const SYNC: Site = Site { name: 0x5CC, head: 0x5E4, count: 0x62C, poll: true, pass: 35, not_yet: 0, ret: (0x7B4, 0x7E0) };
const READY: Site = Site { name: 0x84C, head: 0x864, count: 0x8AC, poll: true, pass: 38, not_yet: 0, ret: (0xA7C, 0xAA8) };
const CW: Site = Site { name: 0xC90, head: 0xCC0, count: 0xD08, poll: true, pass: 30, not_yet: 0, ret: (0xE90, 0xEB4) };
const DATASYNC: Site = Site { name: 0x1380, head: 0x1398, count: 0x13D8, poll: false, pass: 30, not_yet: 1, ret: (0x1478, 0x1494) };

/// What the waits' calls of libetc's `VSync(-1)` and `CheckCallback()`
/// spend: their originals' paths (`vsync.o` +0h..+5Ch, +60h..+6Ch,
/// +164h..+174h; `intr.o` +154h..+160h), as libetc's adapters spend them.
const VCOUNT: u64 = 33;
const CHECK: u64 = 4;
/// What a wait's look in a callback adds when the controller presents
/// nothing: the poll's blocks around its loop, a pass of the loop and
/// `getintr`'s look (+0h..+48h, +54Ch..+560h).
const POLLED: u64 = 6 + 2 + 25 + 3 + 4;

/// `bios_1`'s `.bss`: the result buffers the callbacks get.
mod bs {
    pub const SYNC: u32 = 0x00;
    pub const READY: u32 = 0x08;
    pub const DATA: u32 = 0x10;
}

/// Passes of a libcd wait loop before it gives up.
const TIMEOUT_PASSES: u64 = 0x3C_0000;

const DPCR: u32 = psx_devices::dma::DPCR;
/// The memory controller's CD-ROM and common delays, which the drive layer
/// sets around a sector transfer and `CD_flush` leaves at 1325h.
const CDROM_DELAY: u32 = 0x1F80_1018;
const COM_DELAY: u32 = 0x1F80_1020;
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

/// The interrupt the controller presents: 1-5, or 0.
fn pending(m: &mut M) -> u8 {
    cdc(m, |c, _| c.pending() & 7).unwrap_or(0)
}

fn logged(m: &mut M, what: &str, r: std::result::Result<(), String>) {
    if let Err(e) = r {
        m.log(&format!("{what}: {e}"));
    }
}

type Then = Box<dyn FnOnce(&mut M, u32) -> Flow<Psyq>>;
type Next = Box<dyn FnOnce(&mut M) -> Flow<Psyq>>;

fn call(addr: u32, args: [u32; 4], then: impl FnOnce(&mut M, u64) -> Flow<Psyq> + 'static) -> Flow<Psyq> {
    Flow::Call { call: GuestCall { addr, args }, then: Box::new(then) }
}

// --- Time (D71) --------------------------------------------------------------

/// One of the original's loop headers: where the guest's budget has run
/// out, a safepoint as the link's check there takes it (a quantum's budget
/// added, a quantum passing, what fell due delivered outside a callback);
/// then `k`.
fn header(m: &mut M, k: Next) -> Flow<Psyq> {
    match m.regs.budget() {
        Some(b) if b <= 0 => {
            m.regs.set_budget(b + psx_bios::BUDGET as i32);
            let until = m.clock.now + quantum(m);
            m.run_until(until);
            Flow::Block { until: Wait::Cycle(until), then: k }
        }
        _ => k(m),
    }
}

/// `n` passes of one of the original's loops whose blocks are `body`, each
/// from its header ([`header`]); then `k`.
fn passes(m: &mut M, n: u32, body: (u32, u32), k: Next) -> Flow<Psyq> {
    for i in 0..n {
        if m.regs.budget().is_some_and(|b| b <= 0) {
            return header(
                m,
                Box::new(move |m| {
                    crate::path(m, &[body]);
                    passes(m, n - i - 1, body, k)
                }),
            );
        }
        crate::path(m, &[body]);
    }
    k(m)
}

/// Copy a result buffer to `to` (8 bytes), if there is one, as the
/// original's byte loop does: `pre`, then 8 passes from its header at `at`
/// ([`passes`]); then `k`.
fn copy_result(m: &mut M, from: u32, to: u32, pre: (u32, u32), at: u32, k: Next) -> Flow<Psyq> {
    if to == 0 {
        return k(m);
    }
    let v = m.mem.bytes(from, 8);
    m.mem.write(to, &v);
    crate::path(m, &[pre]);
    passes(m, 8, (at, at + 0x14), k)
}

// --- The interrupt ---------------------------------------------------------

/// `bios_1`'s interrupt routine (+0h): the interrupt the controller
/// presents, its response, acknowledged; the drive status from it; the
/// interrupt states and result buffers. What to call: bit 1 the sync
/// callback, bit 2 the ready callback (0: nothing presented). It runs in
/// the exception, its blocks spent there, a quantum passing at its loops'
/// headers once the budget has run out ([`crate::irq_header`]): the flags
/// read again (+64h), the response's bytes (+7Ch), the rest of the buffer
/// zeroed (+D4h), and each copy of the buffer.
fn getintr(m: &mut M, b: Bios1) -> std::result::Result<u32, String> {
    let intr = cdc(m, |c, _| c.pending())? & 7;
    crate::path(m, &[(0x0, 0x48)]);
    if intr == 0 {
        crate::path(m, &[(0x54C, 0x560)]);
        return Ok(0);
    }
    // The flags again until two reads agree, as the controller's do at once.
    crate::path(m, &[(0x4C, 0x50)]);
    crate::irq_header(m);
    crate::path(m, &[(0x64, 0x74), (0x78, 0x78)]);
    let resp = cdc(m, |c, _| c.take_response(8))?;
    let n = resp.len();
    for _ in 0..n {
        crate::irq_header(m);
        crate::path(m, &[(0x7C, 0x98), (0x9C, 0xBC)]);
    }
    if n < 8 {
        crate::irq_header(m);
        crate::path(m, &[(0x7C, 0x98)]);
    }
    crate::path(m, &[(0xC0, 0xC8)]);
    if n < 8 {
        crate::path(m, &[(0xCC, 0xD0)]);
        for _ in n..8 {
            crate::irq_header(m);
            crate::path(m, &[(0xD4, 0xE4)]);
        }
    }
    crate::path(m, &[(0xE8, 0x124)]);
    cdc(m, |c, _| {
        c.acknowledge();
        c.set_enable(7);
    })?;
    let mut buf = [0u8; 8];
    buf[..n].copy_from_slice(&resp);
    let com = m.mem.r8(b.data + d::COM);
    if intr == 3 {
        crate::path(m, &[(0x128, 0x14C)]);
    }
    let mut stat = 0;
    if intr != 3 || b.table(m, d::ACK_STATUS, com) != 0 {
        crate::path(m, &[(0x150, 0x164)]);
        if b.word(m, d::STATUS) & 0x10 == 0 {
            crate::path(m, &[(0x168, 0x178)]);
            if buf[0] & 0x10 != 0 {
                crate::path(m, &[(0x17C, 0x190)]);
                let n = b.word(m, d::NOPEN);
                m.mem.w32(b.data + d::NOPEN, n.wrapping_add(1));
            }
        }
        crate::path(m, &[(0x194, 0x1B0)]);
        stat = buf[0] & 0x1D;
        m.mem.w32(b.data + d::STATUS, buf[0] as u32);
        m.mem.w32(b.data + d::STATUS1, buf[1] as u32);
    }
    crate::path(m, &[(0x1B4, 0x1C0)]);
    if intr == 5 {
        crate::path(m, &[(0x1C4, 0x1D8)]);
    }
    crate::path(m, &[(0x234, 0x248)]);
    if !(1..=5).contains(&intr) {
        crate::path(m, &[(0x530, 0x538), (0x53C, 0x548), (0x54C, 0x560)]);
        m.log(&format!("CDROM: unknown intr ({intr})"));
        return Ok(0);
    }
    crate::path(m, &[(0x24C, 0x260)]);
    // The buffer into a result buffer: 8 passes from the copy's header.
    let put = |m: &mut M, to: u32, at: u32| {
        m.mem.write(b.bss + to, &buf);
        for _ in 0..8 {
            crate::irq_header(m);
            crate::path(m, &[(at, at + 0x14)]);
        }
    };
    let f = match intr {
        3 => {
            crate::path(m, &[(0x264, 0x268)]);
            let (s, f) = if stat != 0 {
                crate::path(m, &[(0x26C, 0x284), (0x288, 0x28C)]);
                put(m, bs::SYNC, 0x290);
                crate::path(m, &[(0x2A8, 0x2AC)]);
                (5, 2)
            } else {
                crate::path(m, &[(0x2B0, 0x2D4)]);
                if b.table(m, d::SECOND, com) != 0 {
                    crate::path(m, &[(0x2D8, 0x2F0), (0x2F4, 0x2F8)]);
                    put(m, bs::SYNC, 0x2FC);
                    crate::path(m, &[(0x314, 0x318)]);
                    (3, 1)
                } else {
                    crate::path(m, &[(0x31C, 0x338), (0x33C, 0x340)]);
                    put(m, bs::SYNC, 0x344);
                    crate::path(m, &[(0x35C, 0x360)]);
                    (2, 2)
                }
            };
            b.set_intr(m, 0, s);
            f
        }
        2 => {
            crate::path(m, &[(0x364, 0x368)]);
            if stat != 0 {
                crate::path(m, &[(0x36C, 0x36C)]);
            }
            b.set_intr(m, 0, if stat != 0 { 5 } else { 2 });
            crate::path(m, &[(0x370, 0x384), (0x388, 0x38C)]);
            put(m, bs::SYNC, 0x390);
            crate::path(m, &[(0x3A8, 0x3AC)]);
            2
        }
        1 => {
            // A lone status byte with an error bit is no error.
            crate::path(m, &[(0x3B0, 0x3B4)]);
            if stat != 0 {
                crate::path(m, &[(0x3B8, 0x3BC)]);
                if n == 1 {
                    crate::path(m, &[(0x3C0, 0x3C0)]);
                }
            }
            let err = stat != 0 && n != 1;
            crate::path(m, &[(0x3C4, 0x3C8)]);
            if err {
                crate::path(m, &[(0x3CC, 0x3CC)]);
            }
            b.set_intr(m, 1, if err { 5 } else { 1 });
            crate::path(m, &[(0x3D0, 0x3E8), (0x3EC, 0x3F0)]);
            put(m, bs::READY, 0x3F4);
            crate::path(m, &[(0x40C, 0x42C)]);
            cdc(m, |c, _| c.request(false))?;
            4
        }
        4 => {
            b.set_intr(m, 2, 4);
            b.set_intr(m, 1, 4);
            crate::path(m, &[(0x430, 0x458), (0x45C, 0x45C)]);
            put(m, bs::DATA, 0x460);
            crate::path(m, &[(0x478, 0x484), (0x488, 0x48C)]);
            put(m, bs::READY, 0x490);
            crate::path(m, &[(0x4A8, 0x4AC)]);
            4
        }
        _ => {
            b.set_intr(m, 1, 5);
            b.set_intr(m, 0, 5);
            crate::path(m, &[(0x4B0, 0x4D8), (0x4DC, 0x4DC)]);
            put(m, bs::SYNC, 0x4E0);
            crate::path(m, &[(0x4F8, 0x504), (0x508, 0x50C)]);
            put(m, bs::READY, 0x510);
            crate::path(m, &[(0x528, 0x52C)]);
            6
        }
    };
    crate::path(m, &[(0x550, 0x560)]);
    Ok(f)
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

/// `bios_1`'s interrupt handler (+14A4h), bound by its place: while the
/// controller presents an interrupt, its callbacks (a data-ready one, with
/// a sync one after it when both are due, or the sync one).
fn handler(m: &mut M, _a: [u32; 4]) -> Result {
    let b = bios1(m, at::HANDLER)?;
    crate::path(m, &[(0x14A4, 0x14D4)]);
    Ok(drain(
        m,
        b,
        0x14D8,
        Box::new(|m| {
            crate::path(m, &[(0x1550, 0x1578)]);
            Flow::ret(0)
        }),
    ))
}

/// The loop that takes what the controller presents and calls its
/// callbacks, from its header at `at`: the handler's (+14D8h), and a
/// wait's in a callback (`CheckCallback()`), laid out alike. It runs in
/// the exception: a quantum passes at its header once the budget has run
/// out ([`crate::irq_header`]). Then `then`.
fn drain(m: &mut M, b: Bios1, at: u32, then: Next) -> Flow<Psyq> {
    crate::irq_header(m);
    crate::path(m, &[(at, at + 4)]);
    let f = getintr(m, b).unwrap_or_else(|e| {
        m.log(&format!("libcd interrupt: {e}"));
        0
    });
    crate::path(m, &[(at + 8, at + 0x10)]);
    if f == 0 {
        return then(m);
    }
    crate::path(m, &[(at + 0x14, at + 0x18)]);
    // The sync callback when it is due and set, then the loop again.
    let sync = move |m: &mut M| -> Flow<Psyq> {
        crate::path(m, &[(at + 0x44, at + 0x48)]);
        if f & 2 == 0 {
            return drain(m, b, at, then);
        }
        crate::path(m, &[(at + 0x4C, at + 0x5C)]);
        match callbacks(m, b, 2).1 {
            Some(c) => {
                crate::path(m, &[(at + 0x60, at + 0x6C)]);
                call(c.addr, c.args, move |m, _| {
                    crate::path(m, &[(at + 0x70, at + 0x74)]);
                    drain(m, b, at, then)
                })
            }
            None => drain(m, b, at, then),
        }
    };
    if f & 4 == 0 {
        return sync(m);
    }
    crate::path(m, &[(at + 0x1C, at + 0x2C)]);
    match callbacks(m, b, 4).0 {
        Some(c) => {
            crate::path(m, &[(at + 0x30, at + 0x3C)]);
            call(c.addr, c.args, move |m, _| {
                crate::path(m, &[(at + 0x40, at + 0x40)]);
                sync(m)
            })
        }
        None => {
            crate::path(m, &[(at + 0x40, at + 0x40)]);
            sync(m)
        }
    }
}

// --- Waits ------------------------------------------------------------------

/// A wait's look, past its loop's common blocks, spending its own: the
/// result when it holds, else `None`, the blocks back to the header spent.
type Done = fn(&mut M, Bios1) -> Option<u32>;

/// One of the drive layer's wait loops (`CD_sync`, `CD_ready`, `CD_cw`,
/// `CD_datasync`): until `done` gives the result, or with `once` after one
/// look (`site.not_yet`). Each pass spends the loop's blocks and the
/// `VSync(-1)` and `CheckCallback()` it calls (D71); the passes until the
/// guest's budget runs out see the same, then its header takes a
/// safepoint, where the interrupts are taken (inside a callback the
/// controller is polled instead, [`drain`]). After [`TIMEOUT_PASSES`]: "CD
/// timeout", `CD_flush`, -1. `then` gets the result, and spends the rest of
/// the function when `done` gave it.
fn wait(m: &mut M, b: Bios1, site: Site, once: bool, done: Done, then: Then) -> Flow<Psyq> {
    pass(m, b, site, once, TIMEOUT_PASSES + 1, done, then)
}

/// The time-out (`site.count` + 4h on): "CD timeout: " through the game's
/// `puts`, then "%s:(%s) Sync=%s, Ready=%s\n" through its `printf` (the
/// wait's name, the command's, the two interrupt states'), `CD_flush`, and
/// -1.
fn timeout(m: &mut M, b: Bios1, site: Site, then: Then) -> Flow<Psyq> {
    let (t, c) = (b.text, site.count);
    let p = t + c + 8;
    let name = hi_lo(m, t + site.name, t + site.name + 4).unwrap_or(0);
    let names = |m: &M, table: u32, i: u8| m.mem.r32(b.data + table + 4 * i as u32);
    let com = m.mem.r8(b.data + d::COM);
    let args = [name, names(m, d::COM_NAMES, com), names(m, d::INTR_NAMES, b.intr(m, 0)), names(m, d::INTR_NAMES, b.intr(m, 1))];
    crate::path(m, &[(c + 4, c + 0xC)]);
    crate::say(m, (p - 4, p + 4, p), &[], move |m| {
        crate::path(m, &[(c + 0x10, c + 0x58)]);
        crate::say(m, (p + 0x48, p + 0x50, p + 0x4C), &args, move |m| {
            crate::path(m, &[(c + 0x5C, c + 0x60)]);
            flush(
                m,
                b,
                at::CD_FLUSH,
                Box::new(move |m| {
                    crate::path(m, &[(0xFC4, 0x1010), (c + 0x64, c + 0x68), (c + 0x70, c + 0x74), site.ret]);
                    then(m, u32::MAX)
                }),
            )
        })
    })
}

/// A pass of the wait from its header, `left` before its time-out.
fn pass(m: &mut M, b: Bios1, site: Site, once: bool, left: u64, done: Done, then: Then) -> Flow<Psyq> {
    header(
        m,
        Box::new(move |m| {
            let (h, c) = (site.head, site.count);
            crate::path(m, &[(h, h + 4)]);
            crate::take(m, VCOUNT);
            crate::path(m, &[(h + 8, h + 0x1C), (h + 0x20, c)]);
            if left == 0 {
                return timeout(m, b, site, then);
            }
            crate::path(m, &[(c + 0x6C, c + 0x74)]);
            let look: Next = Box::new(move |m| {
                if let Some(v) = done(m, b) {
                    return then(m, v);
                }
                if once {
                    crate::path(m, &[site.ret]);
                    return then(m, site.not_yet);
                }
                let left = left - 1;
                if m.regs.budget().is_none() {
                    // No guest budget (tests): a quantum a pass.
                    let until = m.clock.now + quantum(m);
                    m.run_until(until);
                    return Flow::Block { until: Wait::Cycle(until), then: Box::new(move |m| pass(m, b, site, once, left, done, then)) };
                }
                // The passes that follow see the same while every header in
                // them finds budget left; the one after runs for itself.
                let mut per = site.pass as u64 + VCOUNT;
                if site.poll {
                    per += CHECK + if m.in_irq { POLLED } else { 0 };
                }
                let more = (crate::budget(m).max(0) as u64 / (per * crate::UNIT as u64)).saturating_sub(1).min(left);
                crate::take(m, more * per);
                pass(m, b, site, once, left - more, done, then)
            });
            if !site.poll {
                return look(m);
            }
            crate::path(m, &[(c + 0x78, c + 0x7C)]);
            crate::take(m, CHECK);
            crate::path(m, &[(c + 0x80, c + 0x84)]);
            // In a callback (libetc's dispatcher, or any exception) no
            // interrupt is taken: the loop polls the controller itself.
            if !m.in_irq {
                return look(m);
            }
            crate::path(m, &[(c + 0x88, c + 0x9C)]);
            header(
                m,
                Box::new(move |m| {
                    drain(
                        m,
                        b,
                        c + 0xA0,
                        Box::new(move |m| {
                            crate::path(m, &[(c + 0x118, c + 0x124)]);
                            look(m)
                        }),
                    )
                }),
            )
        }),
    )
}

/// `CD_sync(mode, result)` (+564h): the command's completion (2) or error
/// (5), its result copied, the state back to complete. Without waiting:
/// 0 if neither.
fn sync(m: &mut M, b: Bios1, once: bool, result: u32, then: Then) -> Flow<Psyq> {
    crate::path(m, &[(0x564, 0x59C)]);
    crate::take(m, VCOUNT);
    crate::path(m, &[(0x5A0, 0x5E0)]);
    let done: Done = |m, b| {
        crate::path(m, &[(0x754, 0x764)]);
        let s = b.intr(m, 0);
        if s == 2 {
            return Some(2);
        }
        crate::path(m, &[(0x768, 0x76C)]);
        if s == 5 {
            return Some(5);
        }
        crate::path(m, &[(0x7AC, 0x7B0)]);
        None
    };
    let then: Then = Box::new(move |m, v| {
        if v != 2 && v != 5 {
            return then(m, v);
        }
        b.set_intr(m, 0, 2);
        crate::path(m, &[(0x770, 0x784)]);
        let out: Next = Box::new(move |m| {
            crate::path(m, &[(0x7A4, 0x7A8), SYNC.ret]);
            then(m, v)
        });
        copy_result(m, b.bss + bs::SYNC, result, (0x788, 0x788), 0x78C, out)
    });
    wait(m, b, SYNC, once, done, then)
}

/// `CD_ready(mode, result)` (+7E4h): a data-end interrupt (4), else data
/// ready (1) or error (5), taken with its result. Without waiting: 0 if
/// none.
fn ready(m: &mut M, b: Bios1, once: bool, result: u32, then: Then) -> Flow<Psyq> {
    crate::path(m, &[(0x7E4, 0x81C)]);
    crate::take(m, VCOUNT);
    crate::path(m, &[(0x820, 0x860)]);
    let done: Done = |m, b| {
        crate::path(m, &[(0x9D4, 0x9E4)]);
        let end = b.intr(m, 2);
        if end != 0 {
            return Some(0x200 | end as u32);
        }
        crate::path(m, &[(0xA24, 0xA34)]);
        let data = b.intr(m, 1);
        if data != 0 {
            return Some(0x100 | data as u32);
        }
        crate::path(m, &[(0xA74, 0xA78)]);
        None
    };
    let then: Then = Box::new(move |m, v| {
        if v == 0 || v == u32::MAX {
            return then(m, v);
        }
        let out = move |m: &mut M, tail: (u32, u32)| {
            crate::path(m, &[tail, READY.ret]);
            then(m, v & 0xFF)
        };
        if v >> 8 == 2 {
            b.set_intr(m, 2, 0);
            crate::path(m, &[(0x9E8, 0x9F8)]);
            if result == 0 {
                return out(m, (0xA6C, 0xA70));
            }
            return copy_result(m, b.bss + bs::DATA, result, (0x9FC, 0xA00), 0xA04, Box::new(move |m| out(m, (0xA1C, 0xA20))));
        }
        b.set_intr(m, 1, 0);
        crate::path(m, &[(0xA38, 0xA4C)]);
        copy_result(m, b.bss + bs::READY, result, (0xA50, 0xA50), 0xA54, Box::new(move |m| out(m, (0xA6C, 0xA70))))
    });
    wait(m, b, READY, once, done, then)
}

/// `CD_cw(com, param, result, async)` (+AACh): after the last command
/// completes (`CD_sync`), the command to the controller (Setloc and Setmode
/// remembered); unless `async`, its first answer, its result copied: 0, or
/// -1 on an error answer; -2 without a parameter it needs.
fn cw(m: &mut M, b: Bios1, com: u8, param: u32, result: u32, nowait: bool, then: Then) -> Flow<Psyq> {
    crate::path(m, &[(0xAAC, 0xAEC), (0xB10, 0xB2C)]);
    let n = b.table(m, d::NPARAMS, com);
    if n != 0 {
        crate::path(m, &[(0xB30, 0xB34)]);
        if param == 0 {
            crate::path(m, &[(0xB38, 0xB48), CW.ret]);
            return then(m, (-2i32) as u32);
        }
    }
    crate::path(m, &[(0xB6C, 0xB70)]);
    let issue = move |m: &mut M, _| {
        crate::path(m, &[(0xB74, 0xB80)]);
        if com != com::SETLOC {
            return command(m, b, com, param, result, nowait, n, then);
        }
        let pos = m.mem.bytes(param, 4);
        m.mem.write(b.data + d::POS, &pos);
        crate::path(m, &[(0xB84, 0xB88)]);
        let rest: Next = Box::new(move |m| {
            crate::path(m, &[(0xBAC, 0xBB0)]);
            command(m, b, com, param, result, nowait, n, then)
        });
        passes(m, 4, (0xB8C, 0xBA8), rest)
    };
    sync(m, b, false, 0, Box::new(issue))
}

/// `CD_cw` past Setloc's position (+BB4h): Setmode's mode kept, the
/// states, the `n` parameters and the command to the controller; unless
/// `nowait`, its first answer.
#[allow(clippy::too_many_arguments)]
fn command(m: &mut M, b: Bios1, com: u8, param: u32, result: u32, nowait: bool, n: u32, then: Then) -> Flow<Psyq> {
    crate::path(m, &[(0xBB4, 0xBB8)]);
    if com == com::SETMODE {
        crate::path(m, &[(0xBBC, 0xBC4)]);
        let mode = m.mem.r8(param);
        m.mem.w8(b.data + d::MODE, mode);
    }
    crate::path(m, &[(0xBC8, 0xBEC)]);
    b.set_intr(m, 0, 0);
    if b.table(m, d::READY_RESET, com) != 0 {
        crate::path(m, &[(0xBF0, 0xBF0)]);
        b.set_intr(m, 1, 0);
    }
    crate::path(m, &[(0xBF4, 0xC18)]);
    let n = if (n as i32) > 0 { n.min(16) } else { 0 };
    let params = m.mem.bytes(param, n as usize);
    let send: Next = Box::new(move |m| {
        crate::path(m, &[(0xC4C, 0xC64)]);
        m.mem.w8(b.data + d::COM, com);
        let r = cdc(m, |c, now| c.issue(com, &params, now));
        logged(m, "CD_cw", r);
        if nowait {
            crate::path(m, &[CW.ret]);
            return then(m, 0);
        }
        let answer: Then = Box::new(move |m, v| {
            if v == u32::MAX {
                return then(m, v);
            }
            crate::path(m, &[(0xE40, 0xE4C)]);
            let out: Next = Box::new(move |m| {
                crate::path(m, &[(0xE6C, 0xE84)]);
                let err = b.intr(m, 0) == 5;
                if err {
                    crate::path(m, &[(0xE88, 0xE8C)]);
                }
                crate::path(m, &[CW.ret]);
                then(m, if err { u32::MAX } else { 0 })
            });
            copy_result(m, b.bss + bs::SYNC, result, (0xE50, 0xE50), 0xE54, out)
        });
        crate::path(m, &[(0xC68, 0xC6C)]);
        crate::take(m, VCOUNT);
        crate::path(m, &[(0xC70, 0xCA4)]);
        if b.intr(m, 0) != 0 {
            return answer(m, 0);
        }
        crate::path(m, &[(0xCA8, 0xCBC)]);
        let answered: Done = |m, b| {
            crate::path(m, &[(0xE30, 0xE3C)]);
            (b.intr(m, 0) != 0).then_some(0)
        };
        wait(m, b, CW, false, answered, answer)
    });
    if n == 0 {
        return send(m);
    }
    crate::path(m, &[(0xC1C, 0xC20)]);
    passes(m, n, (0xC24, 0xC48), send)
}

/// `CD_flush`'s loop, or `CD_init`'s copy of it, from `at` (+F40h,
/// +11C0h): what the controller presents acknowledged, a pass each from
/// the loop's header (+34h); the states back to idle, the data FIFO
/// emptied, the common delay at 1325h. Then `k`, which spends the rest.
fn flush(m: &mut M, b: Bios1, at: u32, k: Next) -> Flow<Psyq> {
    crate::path(m, &[(at, at + 0x2C)]);
    if pending(m) == 0 {
        return flushed(m, b, k);
    }
    crate::path(m, &[(at + 0x30, at + 0x30)]);
    flush_pass(m, b, at, k)
}

fn flush_pass(m: &mut M, b: Bios1, at: u32, k: Next) -> Flow<Psyq> {
    header(
        m,
        Box::new(move |m| {
            crate::path(m, &[(at + 0x34, at + 0x80)]);
            let r = cdc(m, |c, _| {
                c.acknowledge();
                c.set_enable(7);
            });
            logged(m, "CD_flush", r);
            if pending(m) != 0 {
                return flush_pass(m, b, at, k);
            }
            flushed(m, b, k)
        }),
    )
}

fn flushed(m: &mut M, b: Bios1, k: Next) -> Flow<Psyq> {
    let r = cdc(m, |c, _| c.request(false));
    logged(m, "CD_flush", r);
    b.set_intr(m, 2, 0);
    b.set_intr(m, 1, 0);
    b.set_intr(m, 0, 2);
    let r = set_reg(m, COM_DELAY, 4, 0x1325);
    logged(m, "CD_flush", r);
    k(m)
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

/// `CD_flush`: its result is a register value left in v0.
fn cd_flush(m: &mut M, _a: [u32; 4]) -> Result {
    let b = bios1(m, at::CD_FLUSH)?;
    Ok(flush(
        m,
        b,
        at::CD_FLUSH,
        Box::new(|m| {
            crate::path(m, &[(0xFC4, 0x1010)]);
            value(0x1325)
        }),
    ))
}

/// `CD_vol(vol)`: the CD-to-SPU volume matrix, applied.
fn cd_vol(m: &mut M, a: [u32; 4]) -> Result {
    bios1(m, at::CD_VOL)?;
    crate::path(m, &[(0xEB8, 0xF3C)]);
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
    crate::path(m, &[(0x1014, 0x102C)]);
    if reg(m, SPU + 0x1B8, 2)? != 0 {
        crate::path(m, &[(0x1050, 0x1050)]);
    } else {
        crate::path(m, &[(0x1030, 0x103C)]);
        if reg(m, SPU + 0x1BA, 2)? == 0 {
            crate::path(m, &[(0x1040, 0x104C), (0x1050, 0x1050)]);
            set_reg(m, SPU + 0x180, 2, 0x3FFF)?;
            set_reg(m, SPU + 0x182, 2, 0x3FFF)?;
        }
    }
    crate::path(m, &[(0x1054, 0x1100)]);
    set_reg(m, SPU + 0x1B0, 2, 0x3FFF)?;
    set_reg(m, SPU + 0x1B2, 2, 0x3FFF)?;
    set_reg(m, SPU + 0x1AA, 2, 0xC001)?;
    cdc(m, |c, _| c.set_volume([0x80, 0, 0x80, 0]))?;
    ret(0)
}

/// The callback slots and status cleared, then `ResetCallback()` and
/// `InterruptCallback(2, handler)` as guest calls (the `jal`s at `reset`
/// and `intr` in bios_1's text, the blocks between them spent), so the
/// handler goes in whichever way libetc runs.
fn install(m: &mut M, b: Bios1, reset: u32, intr: u32, then: impl FnOnce(&mut M, u64) -> Flow<Psyq> + 'static) -> Result {
    for o in [d::CBREADY, d::CBSYNC, d::STATUS1, d::STATUS] {
        m.mem.w32(b.data + o, 0);
    }
    let (reset_f, intr_f) = (jal(m, b.text + reset)?, jal(m, b.text + intr)?);
    let handler = b.text + at::HANDLER;
    Ok(call(reset_f, [0; 4], move |m, _| {
        crate::path(m, &[(reset + 8, intr + 4)]);
        call(intr_f, [2, handler, 0, 0], then)
    }))
}

/// `CD_initintr`: the handler installed; `InterruptCallback`'s result.
fn cd_initintr(m: &mut M, _a: [u32; 4]) -> Result {
    let b = bios1(m, at::CD_INITINTR)?;
    crate::path(m, &[(0x1104, 0x112C)]);
    install(m, b, 0x1128, 0x1138, |m, v| {
        crate::path(m, &[(0x1140, 0x114C)]);
        Flow::Return(v & 0xFFFF_FFFF)
    })
}

/// `CD_init`: no command, mode 0, the handler installed, whatever the
/// controller presents acknowledged, the states idle, then Nop (twice
/// with the shell open), Init and Demute, and Init's completion: 0, or -1.
fn cd_init(m: &mut M, _a: [u32; 4]) -> Result {
    let b = bios1(m, at::CD_INIT)?;
    // "CD_init:" through the game's `puts`, then "addr=%08x\n" through its
    // `printf`.
    let t = b.text;
    crate::path(m, &[(0x1150, 0x1164)]);
    Ok(crate::say(m, (t + 0x1154, t + 0x1158, t + 0x1160), &[], move |m| {
        crate::path(m, &[(0x1168, 0x1178)]);
        crate::say(m, (t + 0x1168, t + 0x116C, t + 0x1174), &[b.data + d::INIT_ADDR], move |m| cd_init_rest(m, b).unwrap_or_else(|e| panic!("CD_init: {e}")))
    }))
}

fn cd_init_rest(m: &mut M, b: Bios1) -> Result {
    crate::path(m, &[(0x117C, 0x11AC)]);
    m.mem.w8(b.data + d::COM, 0);
    m.mem.w8(b.data + d::MODE, 0);
    let fail = (-1i32) as u32;
    let out = |m: &mut M, v: u32| {
        crate::path(m, &[(0x1320, 0x132C)]);
        value(v)
    };
    let demute: Then = Box::new(move |m, v| {
        crate::path(m, &[(0x12F4, 0x12F8)]);
        if v != 0 {
            crate::path(m, &[(0x131C, 0x131C)]);
            return out(m, fail);
        }
        crate::path(m, &[(0x12FC, 0x1300)]);
        sync(
            m,
            b,
            false,
            0,
            Box::new(move |m, v| {
                crate::path(m, &[(0x1304, 0x1310)]);
                if v != 2 {
                    return out(m, fail);
                }
                crate::path(m, &[(0x1314, 0x1318)]);
                out(m, 0)
            }),
        )
    });
    let init: Then = Box::new(move |m, v| {
        crate::path(m, &[(0x12D8, 0x12DC)]);
        if v != 0 {
            return out(m, fail);
        }
        crate::path(m, &[(0x12E0, 0x12F0)]);
        cw(m, b, com::DEMUTE, 0, 0, false, demute)
    });
    let after_nop: Then = Box::new(move |m, _| {
        crate::path(m, &[(0x129C, 0x12B0)]);
        let start = move |m: &mut M| {
            crate::path(m, &[(0x12C4, 0x12D4)]);
            cw(m, b, com::INIT, 0, 0, false, init)
        };
        if b.word(m, d::STATUS) & 0x10 != 0 {
            crate::path(m, &[(0x12B4, 0x12C0)]);
            cw(m, b, com::NOP, 0, 0, false, Box::new(move |m, _| start(m)))
        } else {
            start(m)
        }
    });
    install(m, b, 0x11A8, 0x11B8, move |m, _| {
        flush(
            m,
            b,
            0x11C0,
            Box::new(move |m| {
                crate::path(m, &[(0x1244, 0x1298)]);
                cw(m, b, com::NOP, 0, 0, false, after_nop)
            }),
        )
    })
}

/// `CD_datasync(mode)`: 0 once DMA3 is idle; with `mode`, 1 while it is
/// busy, after one look; mode 0 waits, as libcd's other waits do (its
/// timeout, message and `CD_flush`, -1).
fn cd_datasync(m: &mut M, a: [u32; 4]) -> Result {
    let b = bios1(m, at::CD_DATASYNC)?;
    crate::path(m, &[(0x1330, 0x1354)]);
    crate::take(m, VCOUNT);
    crate::path(m, &[(0x1358, 0x1394)]);
    let idle: Done = |m, _| {
        crate::path(m, &[(0x1450, 0x146C)]);
        if !reg(m, CHCR3, 4).is_ok_and(|c| c & CHCR_BUSY != 0) {
            return Some(0);
        }
        crate::path(m, &[(0x1470, 0x1474)]);
        None
    };
    let then: Then = Box::new(|m, v| {
        if v == 0 {
            crate::path(m, &[DATASYNC.ret]);
        }
        value(v)
    });
    Ok(wait(m, b, DATASYNC, a[0] != 0, idle, then))
}

// --- bios_2, bios_3 ---------------------------------------------------------

/// DMA3 from the controller's data FIFO, as the drive layer sets it up: the
/// sector buffer into the FIFO, the memory controller's CD-ROM and common
/// delays, channel 3 enabled, its address and `words` words, through the
/// DMA controller's registers (shared with the other chips: completion
/// flags DICR and raises IRQ3 when enabled, as on the device); then
/// [`transfer`].
fn getsector(m: &mut M, madr: u32, words: u32, delays: (u32, u32)) -> std::result::Result<(), String> {
    cdc(m, |c, _| c.request(true))?;
    set_reg(m, CDROM_DELAY, 4, delays.0)?;
    set_reg(m, COM_DELAY, 4, delays.1)?;
    let dpcr = reg(m, DPCR, 4)?;
    set_reg(m, DPCR, 4, dpcr | 0x8000)?;
    set_reg(m, MADR3, 4, madr)?;
    set_reg(m, BCR3, 4, words | 0x1_0000)
}

/// The transfer started with `chcr`, and the common delay after it.
fn transfer(m: &mut M, chcr: u32, after: Option<u32>) -> std::result::Result<(), String> {
    set_reg(m, CHCR3, 4, chcr)?;
    m.cdc_sync();
    if let Some(d) = after {
        set_reg(m, COM_DELAY, 4, d)?;
    }
    Ok(())
}

/// `CD_getsector(madr, words)`: its wait for the FIFO (+8Ch) a loop's
/// header, the FIFO holding the sector; the transfer done at once.
fn cd_getsector(m: &mut M, a: [u32; 4]) -> Result {
    getsector(m, a[0], a[1], (0x2_0943, 0x1323))?;
    crate::path(m, &[(0x0, 0x88)]);
    Ok(header(
        m,
        Box::new(|m| {
            crate::path(m, &[(0x8C, 0x9C), (0xA0, 0xCC), (0xE8, 0xFC)]);
            let r = transfer(m, 0x1100_0000, Some(0x1325));
            logged(m, "CD_getsector", r);
            value(0)
        }),
    ))
}

fn cd_getsector2(m: &mut M, a: [u32; 4]) -> Result {
    getsector(m, a[0], a[1], (0x2102_0843, 0x1325))?;
    crate::path(m, &[(0x0, 0x9C), (0xB4, 0xE8)]);
    transfer(m, 0x1140_0100, None)?;
    ret(0)
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

    /// Streaming from memory (`StSetEmulate`): the next sector, the
    /// original's count `step` when it is.
    fn emu_step(self, m: &mut M, step: (u32, u32)) {
        if m.mem.r32(self.emu_addr) != 0 {
            crate::path(m, &[step]);
            let i = m.mem.r32(self.emu_idx);
            m.mem.w32(self.emu_idx, i.wrapping_add(1));
        }
    }
}

/// `StCdInterrupt`'s epilogue.
const END: (u32, u32) = (0x90C, 0x918);

/// `c_011`'s DMA helper (+948h): channel 3's interrupt enabled for a
/// frame's last sector and disabled otherwise (a byte store to DICR, as
/// the original makes it), the channel enabled, then the transfer. Its
/// blocks with DMA3 idle and the FIFO holding the sector, which its waits
/// find at once.
fn ring_dma(m: &mut M, madr: u32, words: u32, chcr: u32, irq: bool) -> std::result::Result<(), String> {
    let enables = if irq { (0x9CC, 0x9E4) } else { (0xA08, 0xA1C) };
    crate::path(m, &[(0x948, 0x998), (0x9C0, 0x9C8), enables, (0xA20, 0xAA0), (0xAB8, 0xAEC)]);
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

/// `c_011`'s word copy (+91Ch) of `n` words from `from` to `to`: its
/// blocks, a pass a word from its loop's header; then `k`.
fn ring_copy(m: &mut M, to: u32, from: u32, n: u32, k: Next) -> Flow<Psyq> {
    let v = m.mem.bytes(from, 4 * n as usize);
    m.mem.write(to, &v);
    crate::path(m, &[(0x91C, 0x920)]);
    passes(
        m,
        n,
        (0x924, 0x93C),
        Box::new(move |m| {
            crate::path(m, &[(0x940, 0x944)]);
            k(m)
        }),
    )
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
/// below the caller's stack that holds `CdReady`'s result. Its blocks on
/// that path (D71), its loops' headers taking safepoints.
fn st_cd_interrupt(m: &mut M, _a: [u32; 4]) -> Result {
    let s = Ring::find(m, m.sdk.at)?;
    crate::path(m, &[(0x0, 0x14)]);
    if m.mem.r32(s.final_sector) == 1 {
        crate::path(m, &[END]);
        return ret(0);
    }
    crate::path(m, &[(0x18, 0x28)]);
    if m.mem.r32(s.rgb24) != 0 {
        crate::path(m, &[(0x2C, 0x48)]);
        // 24-bit movies skip sectors while the GPU's DMA (the last frame)
        // runs.
        let gpu = m.mem.r32(s.data + 0x20);
        if reg(m, gpu, 4)? & CHCR_BUSY != 0 {
            crate::path(m, &[(0x4C, 0x5C)]);
            m.mem.w32(s.intr_flag, 1);
            s.emu_step(m, (0x60, 0x74));
            crate::path(m, &[(0x78, 0x80), END]);
            s.status(m, 1);
            return ret(0);
        }
    }
    crate::path(m, &[(0x84, 0x88)]);
    let frame = m.regs.sp().wrapping_sub(64);
    m.regs.set_sp(frame);
    Ok(call(s.cd_ready, [1, frame + 48, 0, 0], move |m, v| ring_sector(m, s, frame, v as u32)))
}

/// The end of `StCdInterrupt` (+90Ch), with its status `code` if it sets
/// one: its stack frame popped.
fn done(m: &mut M, s: Ring, frame: u32, code: Option<u32>) -> Flow<Psyq> {
    if let Some(c) = code {
        s.status(m, c);
    }
    crate::path(m, &[END]);
    leave(m, frame)
}

fn leave(m: &mut M, frame: u32) -> Flow<Psyq> {
    m.regs.set_sp(frame.wrapping_add(64));
    Flow::ret(0)
}

/// `StCdInterrupt` after `CdReady` (+8Ch): the entry, the request, and
/// the sector's header bytes.
fn ring_sector(m: &mut M, s: Ring, frame: u32, ready: u32) -> Flow<Psyq> {
    crate::path(m, &[(0x8C, 0x94)]);
    if ready == 5 {
        return done(m, s, frame, None);
    }
    crate::path(m, &[(0x98, 0xB8)]);
    if m.mem.r8(frame + 48) & 4 != 0 {
        crate::path(m, &[(0xBC, 0xC4)]);
        return done(m, s, frame, Some(3));
    }
    crate::path(m, &[(0xC8, 0xF4)]);
    let e = m.mem.r32(s.addr).wrapping_add(m.mem.r32(s.idx1).wrapping_mul(32));
    m.mem.w32(s.entry, e);
    if m.mem.r16(e) != 0 {
        // The ring is full.
        crate::path(m, &[(0xF8, 0x108)]);
        s.emu_step(m, (0x10C, 0x124));
        crate::path(m, &[(0x128, 0x130)]);
        return done(m, s, frame, Some(4));
    }
    crate::path(m, &[(0x134, 0x1A4)]);
    let r = cdc(m, |c, _| {
        c.request(false);
        c.request(true);
    });
    logged(m, "StCdInterrupt", r);
    let next: Next = Box::new(move |m| ring_header(m, s, frame));
    if m.mem.r32(s.mode) != 0 {
        return next(m);
    }
    // The header's 4 bytes kept and 8 more read, a byte a pass.
    let h = cdc(m, |c, _| {
        let h = c.read_data(4);
        c.read_data(8);
        h
    })
    .unwrap_or_default();
    m.mem.write(frame + 40, &h);
    crate::path(m, &[(0x1A8, 0x1A8)]);
    passes(
        m,
        4,
        (0x1AC, 0x1CC),
        Box::new(move |m| {
            crate::path(m, &[(0x1D0, 0x1DC)]);
            passes(m, 8, (0x1E0, 0x1F0), next)
        }),
    )
}

/// +1F4h: the frame header into the entry, from memory or by DMA3, then
/// the header's bytes after it.
fn ring_header(m: &mut M, s: Ring, frame: u32) -> Flow<Psyq> {
    crate::path(m, &[(0x1F4, 0x204)]);
    let e = m.mem.r32(s.entry);
    let emu = m.mem.r32(s.emu_addr);
    if emu != 0 {
        let from = emu.wrapping_add(m.mem.r32(s.emu_idx).wrapping_mul(2048));
        crate::path(m, &[(0x208, 0x228)]);
        return ring_copy(
            m,
            e,
            from,
            8,
            Box::new(move |m| {
                crate::path(m, &[(0x22C, 0x230)]);
                ring_frame(m, s, frame)
            }),
        );
    }
    crate::path(m, &[(0x234, 0x254)]);
    let r = ring_dma(m, e, 8, 0x1100_0000, false);
    logged(m, "StCdInterrupt", r);
    ring_frame(m, s, frame)
}

/// +258h: DMA3 idle (the header's transfer is done at once), the header's
/// bytes in the entry; the start frame, the sector's kind and channel, and
/// its place in the frame.
fn ring_frame(m: &mut M, s: Ring, frame: u32) -> Flow<Psyq> {
    crate::path(m, &[(0x258, 0x274), (0x290, 0x2E0)]);
    let e = m.mem.r32(s.entry);
    let h = m.mem.bytes(frame + 40, 4);
    m.mem.write(e + 0x1C, &h);
    if m.mem.r32(s.start_flag) == 1 {
        crate::path(m, &[(0x2E4, 0x2F4)]);
        let start = m.mem.r32(s.start_frame);
        if start != 0 {
            crate::path(m, &[(0x2F8, 0x310)]);
            if start != m.mem.r16(e + 8) as u32 {
                crate::path(m, &[(0x314, 0x328)]);
                m.mem.w16(e, 0);
                s.emu_step(m, (0x32C, 0x344));
                return done(m, s, frame, None);
            }
            crate::path(m, &[(0x348, 0x34C)]);
            m.mem.w32(s.start_flag, 0);
        }
    }
    crate::path(m, &[(0x350, 0x368)]);
    let e = m.mem.r32(s.entry);
    let other = m.mem.r16(e) != 0x160 || {
        crate::path(m, &[(0x36C, 0x384)]);
        (m.mem.r16(e + 2) >> 10 & 0x1F) as u32 != m.mem.r32(s.cchannel)
    };
    if other {
        crate::path(m, &[(0x388, 0x398)]);
        if m.mem.r32(s.emu_addr) != 0 {
            crate::path(m, &[(0x39C, 0x3A4)]);
            m.mem.w32(s.emu_idx, 0);
        } else {
            crate::path(m, &[(0x3A8, 0x3A8)]);
        }
        crate::path(m, &[(0x3AC, 0x3C4)]);
        m.mem.w16(e, 0);
        return done(m, s, frame, Some(5));
    }
    crate::path(m, &[(0x3C8, 0x3DC)]);
    let offset = m.mem.r16(s.sector_offset) as i16 as i32 as u32;
    let frame_no = m.mem.r32(s.frame_no);
    let out_of_sequence = offset != m.mem.r16(e + 4) as u32 || {
        crate::path(m, &[(0x3E0, 0x3F0)]);
        frame_no != 0 && {
            crate::path(m, &[(0x3F4, 0x400)]);
            frame_no != m.mem.r16(e + 8) as u32
        }
    };
    if out_of_sequence {
        crate::path(m, &[(0x404, 0x428)]);
        return restart(
            m,
            s,
            Box::new(move |m| {
                crate::path(m, &[(0x42C, 0x458)]);
                s.emu_step(m, (0x45C, 0x474));
                crate::path(m, &[(0x478, 0x480)]);
                done(m, s, frame, Some(6))
            }),
        );
    }
    crate::path(m, &[(0x484, 0x49C)]);
    if m.mem.r16(e + 4) != 0 {
        return ring_data(m, s, frame);
    }
    // A frame's first sector.
    crate::path(m, &[(0x4A0, 0x4C4)]);
    m.mem.w16(s.sector_offset, 0);
    let no = m.mem.r16(e + 8) as u32;
    m.mem.w32(s.frame_no, no);
    let end = m.mem.r32(s.end_frame);
    if end != 0 && {
        crate::path(m, &[(0x4C8, 0x4CC)]);
        no >= end
    } {
        // Past the last frame: the stream ends.
        crate::path(m, &[(0x4D0, 0x4F4)]);
        return restart(
            m,
            s,
            Box::new(move |m| {
                crate::path(m, &[(0x4F8, 0x528)]);
                m.mem.w32(s.start_flag, 1);
                func2(
                    m,
                    s,
                    (0x52C, 0x530),
                    Box::new(move |m| {
                        crate::path(m, &[(0x534, 0x544)]);
                        s.emu_step(m, (0x548, 0x560));
                        crate::path(m, &[(0x564, 0x56C)]);
                        done(m, s, frame, Some(7))
                    }),
                )
            }),
        );
    }
    crate::path(m, &[(0x570, 0x59C)]);
    let first: Next = Box::new(move |m| {
        crate::path(m, &[(0x6BC, 0x6CC)]);
        let i1 = m.mem.r32(s.idx1);
        m.mem.w32(s.idx2, i1);
        ring_data(m, s, frame)
    });
    let room = m.mem.r32(s.size).wrapping_sub(m.mem.r32(s.idx1)).wrapping_sub(1);
    if room >= m.mem.r16(e + 6) as u32 {
        return first(m);
    }
    // The frame doesn't fit before the ring's end.
    crate::path(m, &[(0x5A0, 0x5B0)]);
    if m.mem.r32(s.end_frame) == 0 {
        crate::path(m, &[(0x5B4, 0x5CC)]);
        m.mem.w16(e, 1);
        m.mem.w32(s.start_flag, 1);
        return func2(
            m,
            s,
            (0x5D0, 0x5D4),
            Box::new(move |m| {
                crate::path(m, &[(0x5D8, 0x5E8)]);
                s.emu_step(m, (0x5EC, 0x604));
                crate::path(m, &[(0x608, 0x610)]);
                done(m, s, frame, Some(8))
            }),
        );
    }
    crate::path(m, &[(0x614, 0x62C)]);
    let ring = m.mem.r32(s.addr);
    if m.mem.r16(ring) != 0 {
        crate::path(m, &[(0x630, 0x644)]);
        m.mem.w16(e, 0);
        s.emu_step(m, (0x648, 0x660));
        crate::path(m, &[(0x664, 0x66C)]);
        return done(m, s, frame, Some(9));
    }
    // Wrap: the entry moves to the ring's start, a word a pass.
    crate::path(m, &[(0x670, 0x68C)]);
    m.mem.w16(e, 1);
    m.mem.w32(s.idx1, 0);
    let v = m.mem.bytes(e, 32);
    m.mem.write(ring, &v);
    m.mem.w32(s.entry, ring);
    passes(
        m,
        8,
        (0x690, 0x6A8),
        Box::new(move |m| {
            crate::path(m, &[(0x6AC, 0x6B8)]);
            first(m)
        }),
    )
}

/// The ring's start over: `init_ring_status(StRingIdx2, StRingIdx1 -
/// StRingIdx2)` (its entries from StRingIdx2 cleared), the frame and
/// sector counts 0 before it, StRingIdx1 back to StRingIdx2 and the entry
/// cleared after; then `then`, which spends the blocks after the call.
fn restart(m: &mut M, s: Ring, then: Next) -> Flow<Psyq> {
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
}

/// `StFunc2()` when it is set, its `jalr` the blocks `jalr`; then `k`.
fn func2(m: &mut M, s: Ring, jalr: (u32, u32), k: Next) -> Flow<Psyq> {
    let f = m.mem.r32(s.func2);
    if f == 0 {
        return k(m);
    }
    crate::path(m, &[jalr]);
    call(f, [0; 4], move |m, _| k(m))
}

/// +6D0h: the sector's 2016 data bytes into the entry's slot, from memory
/// or by DMA3, its interrupt on for a frame's last sector.
fn ring_data(m: &mut M, s: Ring, frame: u32) -> Flow<Psyq> {
    crate::path(m, &[(0x6D0, 0x730)]);
    s.status(m, 10);
    let so = m.mem.r16(s.sector_offset);
    m.mem.w16(s.sector_offset, so.wrapping_add(1));
    let base = m.mem.r32(s.addr).wrapping_add(m.mem.r32(s.size).wrapping_mul(32)).wrapping_add(m.mem.r32(s.idx1).wrapping_mul(2016));
    m.mem.w32(s.base, base);
    let rgb24 = m.mem.r32(s.rgb24) != 0;
    crate::path(m, &[if rgb24 { (0x734, 0x758) } else { (0x75C, 0x774) }, (0x778, 0x794)]);
    let chcr = if rgb24 { 0x1100_0000 } else { 0x1140_0100 };
    let e = m.mem.r32(s.entry);
    let last = m.mem.r16(e + 6) as i32 - 1 == m.mem.r16(e + 4) as i32;
    if last {
        m.mem.w32(s.final_sector, 1);
    }
    crate::path(m, &[if last { (0x798, 0x7AC) } else { (0x838, 0x848) }]);
    let emu = m.mem.r32(s.emu_addr);
    if emu != 0 {
        let i = m.mem.r32(s.emu_idx);
        let from = emu.wrapping_add(i.wrapping_mul(2048)).wrapping_add(32);
        crate::path(m, &[if last { (0x7B0, 0x7D0) } else { (0x84C, 0x86C) }]);
        return ring_copy(
            m,
            base,
            from,
            504,
            Box::new(move |m| {
                crate::path(m, &[if last { (0x7D4, 0x7EC) } else { (0x870, 0x888) }]);
                m.mem.w32(s.emu_idx, i.wrapping_add(1));
                ring_end(m, s, frame, last)
            }),
        );
    }
    crate::path(m, &[if last { (0x7F0, 0x810) } else { (0x88C, 0x8AC) }]);
    let r = ring_dma(m, base, 504, chcr, last);
    logged(m, "StCdInterrupt", r);
    ring_end(m, s, frame, last)
}

/// +814h, +8B0h: after a frame's last sector the counters start over; the
/// entry's status, the next entry; from memory, the frame handed on here,
/// where no DMA interrupt comes.
fn ring_end(m: &mut M, s: Ring, frame: u32, last: bool) -> Flow<Psyq> {
    if last {
        crate::path(m, &[(0x814, 0x834)]);
        m.mem.w16(s.sector_offset, 0);
        m.mem.w32(s.frame_no, 0);
        let ch = m.mem.r32(s.stchannel);
        m.mem.w32(s.cchannel, ch);
    }
    crate::path(m, &[(0x8B0, 0x8EC)]);
    let e = m.mem.r32(s.entry);
    m.mem.w16(e, 3);
    let i1 = m.mem.r32(s.idx1);
    m.mem.w32(s.idx1, i1.wrapping_add(1));
    if m.mem.r32(s.emu_addr) != 0 {
        crate::path(m, &[(0x8F0, 0x900)]);
        if m.mem.r32(s.final_sector) != 0 {
            crate::path(m, &[(0x904, 0x908)]);
            return call(s.data_ready_callback, [0; 4], move |m, _| done(m, s, frame, None));
        }
    }
    done(m, s, frame, None)
}

/// `StUnSetRing()`: in a critical section, the data and ready callbacks
/// off (libds's when it is active, else libcd's), and the data FIFO
/// emptied. The calls are the original's, its blocks around them;
/// `ExitCriticalSection`'s result.
fn st_unset_ring(m: &mut M, _a: [u32; 4]) -> Result {
    let t = m.sdk.at;
    let ds_active = hi_lo(m, t + 0x10, t + 0x14)?;
    let enter = jal(m, t + 0x08)?;
    let ((data, ready), ds) = if m.mem.r32(ds_active) == 1 { ((jal(m, t + 0x24)?, jal(m, t + 0x2C)?), true) } else { ((jal(m, t + 0x3C)?, jal(m, t + 0x44)?), false) };
    let exit = jal(m, t + 0x68)?;
    crate::path(m, &[(0x0, 0xC)]);
    Ok(call(enter, [0; 4], move |m, _| {
        crate::path(m, &[(0x10, 0x20), if ds { (0x24, 0x28) } else { (0x3C, 0x40) }]);
        call(data, [0; 4], move |m, _| {
            crate::path(m, &[if ds { (0x2C, 0x30) } else { (0x44, 0x48) }]);
            call(ready, [0; 4], move |m, _| {
                if ds {
                    crate::path(m, &[(0x34, 0x38)]);
                }
                crate::path(m, &[(0x4C, 0x6C)]);
                let r = cdc(m, |c, _| c.request(false));
                logged(m, "StUnSetRing", r);
                call(exit, [0; 4], |m, v| {
                    crate::path(m, &[(0x70, 0x7C)]);
                    Flow::Return(v & 0xFFFF_FFFF)
                })
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
        crate::libetc::fixture::install(&mut m);
        crate::libetc::fixture::reset(&mut m);
        crate::libetc::fixture::interrupt(&mut m, 2, TEXT + at::HANDLER);
        // The handler as the game's thunk at its address reaches it.
        crate::testing::double(TEXT + at::HANDLER, |m| {
            let caller = std::mem::replace(&mut m.sdk.at, TEXT + at::HANDLER);
            let r = handler(m, [0; 4]);
            crate::testing::finish(m, r);
            m.sdk.at = caller;
        });
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
        crate::libetc::fixture::dma_callback(&mut m, 3, 0x8002_1000);
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
