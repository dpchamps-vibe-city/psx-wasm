//! The BIOS's CD-ROM driver and its `cdrom` device (psx-spx "BIOS CDROM
//! Functions"; OpenBIOS `cdrom/`, which follows the retail BIOS's code),
//! over the CD controller's registers with its timing.
//!
//! - The driver is a state machine its interrupt handlers advance: two
//!   kernel elements at priority 0 (A(90h..93h), the CD-ROM's and DMA's
//!   verifiers and handlers), which read the controller's interrupt and
//!   response, issue the next command, start DMA3 for each sector and
//!   deliver the CD events (class F0000003h: 10h the transfers done, 20h a
//!   command done, 40h data ready, 80h end of data, 8000h an error).
//! - The asynchronous functions (`CdAsyncSeekL`, `CdAsyncGetStatus`,
//!   `CdAsyncReadSector`, `CdAsyncSetMode`) start a command; the blocking
//!   ones (`CdReadSector`, `CdGetStatus`) wait for the events, time passing
//!   as the drive takes it. A CD error there is `SystemError('D', ...)`.
//! - The `cdrom` device reads the path table and one sector of a
//!   directory, as the retail BIOS does: a file is found by its path's
//!   directories in the path table, then its name (`?` wildcards) in its
//!   directory's first sector; `read` reloads the path table each time
//!   and reads whole sectors.
//! - The boot leaves the driver installed (`_96_init`: its handlers in
//!   their chain, its events open, the CD-ROM's and DMA's interrupts
//!   unmasked, its interrupts acknowledged by its handlers); libetc's
//!   `ResetCallback` takes it out (`_96_remove`) before libcd drives the
//!   controller. The boot's own disc reads (SYSTEM.CNF, the executable,
//!   the path table) are the runtime's, through the host.
//!
//! Where it differs from OpenBIOS: the response is read before the
//! interrupt is acknowledged (psx-spx: acknowledging drains it); a busy
//! driver fails a blocking call at once rather than after its retries.

use crate::files::{self, fcb};
use crate::machine::{Flow, Machine, Sdk, Wait, seq};
use psx_devices::{cd, cdc, dma, irq};
use std::collections::VecDeque;

pub const EV_CD: u32 = 0xF000_0003;
pub const ACK: u32 = 0x10;
pub const DNE: u32 = 0x20;
pub const RDY: u32 = 0x40;
pub const END: u32 = 0x80;
pub const ERR: u32 = 0x8000;
const SPECS: [u32; 5] = [ACK, DNE, RDY, END, ERR];

/// The driver's states (OpenBIOS's, the command's number in most).
const IDLE: u32 = 0xFFFF;
const GETSTATUS: u32 = 0x01;
const SETMODE: u32 = 0x0E;
const SEEKL: u32 = 0x15;
const SEEKP: u32 = 0x16;
const SEEKL_SETLOC: u32 = 0xF2;
const READN: u32 = 0xF6;
const READS: u32 = 0xFB;
const READN_PREEMPTED: u32 = 0xE6;
const READS_PREEMPTED: u32 = 0xEB;
const READ_SETMODE: u32 = 0xFE;
const INITIALIZING: u32 = 0xCCC;
const REINIT: u32 = 0xDDD;
const PAUSING: u32 = 0xFFF;

/// The driver's sector buffer and status byte in kernel RAM (OpenBIOS
/// `g_readBuffer`).
pub const BUFFER: u32 = 0xA000_8000;
const STATUS: u32 = 0xA000_7FF0;

const D3_MADR: u32 = dma::BASE + 0x10 * 3;
const CDROM_DELAY: u32 = 0x1F80_1018;
const COM_DELAY: u32 = 0x1F80_1020;

#[derive(Debug, Clone)]
pub struct Driver {
    state: u32,
    preempted: u32,
    got_int3: bool,
    words: u32,
    sectors: i32,
    dmas: i32,
    buf: u32,
    mode: u32,
    err: [u8; 2],
    got_int5: bool,
    /// Whether the handlers acknowledge their interrupt and return from the
    /// exception (`SetCdromIrqAutoAbort`): the CD-ROM's, DMA's.
    pub auto_ack: [u32; 2],
    status_dst: u32,
    init: u8,
    /// The events `_96_init` opened: 10h, 20h, 40h, 80h, 8000h.
    pub events: [u32; 5],
    fs: Fs,
}

impl Default for Driver {
    fn default() -> Self {
        Self {
            state: IDLE,
            preempted: IDLE,
            got_int3: false,
            words: 0,
            sectors: 0,
            dmas: 0,
            buf: 0,
            mode: 0,
            err: [0; 2],
            got_int5: false,
            auto_ack: [0; 2],
            status_dst: 0,
            init: 0,
            events: [0; 5],
            fs: Fs::default(),
        }
    }
}

/// What the device keeps of the disc: the path table, the last directory
/// read, the current directory and `firstfile`'s search.
#[derive(Debug, Clone, Default)]
struct Fs {
    /// (parent, LBA, name) per directory; its ID is its index + 1.
    paths: Vec<(u32, u32, Vec<u8>)>,
    hash: u32,
    dir_id: usize,
    /// (LBA, size, name) per entry of directory `dir_id`'s first sector.
    dir: Vec<(u32, u32, Vec<u8>)>,
    cwd: Vec<u8>,
    found: usize,
    pattern: Vec<u8>,
    first_skip: bool,
    first_dir: usize,
}

fn bcd(v: u32) -> u8 {
    ((v / 10) << 4 | v % 10) as u8
}

fn le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// `SystemError('D', code)` where the console stops in a wait's
/// continuation, which can't return an error.
fn system_error(code: u32, why: &str) -> ! {
    panic!("SystemError('D', {code:#x}): {why}")
}

/// `?` matches any character (OpenBIOS `patternMatches`).
fn matches(name: &[u8], pattern: &[u8]) -> bool {
    let name = name.split(|&c| c == 0).next().unwrap_or(&[]);
    let mut p = pattern.iter();
    for &c in name {
        match p.next() {
            Some(&b'?') => {}
            Some(&q) if q == c => {}
            _ => return false,
        }
    }
    matches!(p.next(), None | Some(b'?') | Some(0))
}

impl<S: Sdk> Machine<S> {
    fn cd_w(&mut self, r: u32, v: u8) {
        let _ = self.reg_write(cdc::BASE + r, 1, v as u32);
    }

    fn cd_r(&mut self, r: u32) -> u8 {
        self.reg_read(cdc::BASE + r, 1).unwrap_or(0) as u8
    }

    fn cd_command(&mut self, c: u8, params: &[u8]) {
        self.cd_w(0, 0);
        for &p in params {
            self.cd_w(2, p);
        }
        self.cd_w(1, c);
    }

    /// The parameter FIFO isn't full.
    fn cd_ready(&mut self) -> bool {
        self.cd_r(0) & 0x10 != 0
    }

    fn cd_event(&mut self, spec: u32) {
        self.deliver_event(EV_CD, spec);
    }

    fn cd_undeliver(&mut self, specs: &[u32]) {
        for &s in specs {
            self.undeliver_event(EV_CD, s);
        }
    }

    /// `initializeCDRomHandlersAndEvents` and `cdromInnerInit`'s kernel
    /// side: the handlers at priority 0, the events open and enabled, the
    /// interrupts acknowledged by the handlers and unmasked, DMA3's
    /// interrupt on, the controller's interrupts acknowledged and enabled,
    /// the state idle.
    pub fn cd_install(&mut self) {
        self.cd_enqueue();
        for (i, spec) in SPECS.into_iter().enumerate() {
            let h = self.open_event(EV_CD, spec, crate::kernel::EV_MODE_NOINTR, 0);
            self.set_ev_status(h, crate::kernel::EV_ENABLED);
            self.kernel.cd.events[i] = h;
        }
        self.kernel.cd.fs.cwd.clear();
        let d = &mut self.kernel.cd;
        d.auto_ack = [1, 1];
        d.state = IDLE;
        d.preempted = IDLE;
        d.got_int3 = false;
        d.got_int5 = false;
        d.init = 1;
        if self.io.is_none() {
            return;
        }
        let _ = self.reg_write(dma::DPCR, 4, 0x9099);
        let dicr = self.reg_read(dma::DICR, 4).unwrap_or(0);
        let _ = self.reg_write(dma::DICR, 4, dicr & 0xFF_FFFF | 0x8800_0000);
        self.cd_w(0, 1);
        self.cd_w(3, 0x1F);
        self.cd_w(0, 1);
        self.cd_w(2, 0x1F);
        let _ = self.reg_write(irq::I_STAT, 4, !(1 << irq::CDROM));
        let mask = self.reg_read(irq::I_MASK, 4).unwrap_or(0);
        let _ = self.reg_write(irq::I_MASK, 4, mask | 1 << irq::CDROM | 1 << irq::DMA);
    }

    /// A(A2h) `EnqueueCdIntr()`: the CD-ROM's element, then DMA's, at
    /// priority 0 (so DMA's first).
    pub fn cd_enqueue(&mut self) {
        use crate::kernel::{CD_DMA_ELEMENT, CD_IO_ELEMENT, kernel_element};
        self.enq_int_rp(0, kernel_element(CD_IO_ELEMENT));
        self.enq_int_rp(0, kernel_element(CD_DMA_ELEMENT));
    }

    /// A(A3h) `DequeueCdIntr()`.
    pub fn cd_dequeue(&mut self) {
        use crate::kernel::{CD_DMA_ELEMENT, CD_IO_ELEMENT, kernel_element};
        self.deq_int_rp(0, kernel_element(CD_IO_ELEMENT));
        self.deq_int_rp(0, kernel_element(CD_DMA_ELEMENT));
    }

    /// A(56h)/A(72h) `_96_remove()`: in a critical section (left so), the
    /// events closed and the handlers out (OpenBIOS `deinitCDRom`).
    pub fn cd_remove(&mut self) {
        self.crit = true;
        for h in std::mem::take(&mut self.kernel.cd.events) {
            if h != 0 {
                self.set_ev_status(h, crate::kernel::EV_UNUSED);
            }
        }
        self.cd_dequeue();
    }

    /// Whether `_96_init`'s events are open, so a blocking call can end.
    fn cd_installed(&mut self) -> Result<(), String> {
        let events = self.kernel.cd.events;
        let ok = events.iter().all(|&h| h != 0 && self.ev(h).is_some_and(|e| e.status != crate::kernel::EV_UNUSED));
        if !ok {
            return Err("the BIOS's CD driver is not installed (_96_remove took it out; _96_init installs it): its events never come".into());
        }
        if self.io.is_none() {
            return Err("the BIOS's CD driver needs the CD controller".into());
        }
        Ok(())
    }

    /// A(78h) `CdAsyncSeekL(msf)`: Setloc, then SeekL; 1, or 0 when busy.
    pub fn cd_seek_l(&mut self, msf: [u8; 3]) -> u32 {
        let st = self.kernel.cd.state;
        let preempting = st == READN_PREEMPTED || st == READS_PREEMPTED;
        if preempting && !self.kernel.cd.got_int3 || !preempting && st != IDLE || !self.cd_ready() {
            return 0;
        }
        self.cd_undeliver(&[DNE, END, ERR, 0x100, 0x200]);
        if preempting {
            self.kernel.cd.preempted = st;
        }
        self.cd_command(0x02, &msf);
        self.kernel.cd.state = SEEKL_SETLOC;
        1
    }

    /// A(7Ch) `CdAsyncGetStatus(dst)`: GetStat, its status to `dst`.
    pub fn cd_get_status(&mut self, dst: u32) -> u32 {
        if self.kernel.cd.state != IDLE {
            return 0;
        }
        self.cd_undeliver(&[RDY, ACK, DNE, END, ERR, 0x100, 0x200]);
        self.kernel.cd.state = GETSTATUS;
        self.kernel.cd.status_dst = dst;
        self.cd_command(0x01, &[]);
        1
    }

    /// A(7Eh) `CdAsyncReadSector(count, dst, mode)`: Setmode, then ReadN
    /// (ReadS with mode bit 8); each sector by DMA3, 800h, 918h or 924h
    /// bytes as the mode says.
    pub fn cd_read(&mut self, count: u32, dst: u32, mode: u32) -> u32 {
        if self.kernel.cd.state != IDLE || (count as i32) <= 0 {
            return 0;
        }
        self.cd_undeliver(&[RDY, ACK, DNE, END, ERR, 0x100, 0x200]);
        let d = &mut self.kernel.cd;
        d.got_int3 = false;
        d.words = match (mode & 0x10 != 0, mode & 0x20 != 0) {
            (false, false) => 0x200,
            (false, true) => 0x249,
            (true, _) => 0x246,
        };
        d.sectors = count as i32;
        d.dmas = count as i32;
        d.buf = dst;
        d.mode = mode;
        if !self.cd_ready() {
            return 0;
        }
        self.kernel.cd.state = READ_SETMODE;
        self.cd_command(0x0E, &[mode as u8]);
        1
    }

    /// A(81h) `CdAsyncSetMode(mode)`.
    pub fn cd_set_mode(&mut self, mode: u32) -> u32 {
        if self.kernel.cd.state != IDLE {
            return 0;
        }
        self.cd_undeliver(&[RDY, ACK, DNE, END, ERR, 0x100, 0x200]);
        self.kernel.cd.mode = mode;
        if !self.cd_ready() {
            return 0;
        }
        self.kernel.cd.state = SETMODE;
        self.cd_command(0x0E, &[mode as u8]);
        1
    }

    /// A(94h) `CdromGetInt5errCode(dst1, dst2)`: the last INT5's status and
    /// error.
    pub fn cd_int5_error(&mut self, dst1: u32, dst2: u32) {
        let [a, b] = self.kernel.cd.err;
        self.mem.w8(dst1, a);
        self.mem.w8(dst2, b);
    }

    fn irq_line(&mut self, bit: u32) -> bool {
        let mask = self.reg_read(irq::I_MASK, 4).unwrap_or(0);
        let stat = self.reg_read(irq::I_STAT, 4).unwrap_or(0);
        mask & stat & 1 << bit != 0
    }

    /// A(90h) the CD-ROM's verifier: with IRQ2 taken, the controller's
    /// interrupt and response, acknowledged, advance the state machine; 1.
    pub fn cd_io_verifier(&mut self) -> u32 {
        if self.io.is_none() || !self.irq_line(irq::CDROM) {
            return 0;
        }
        self.cd_w(0, 1);
        let flags = self.cd_r(3);
        let mut resp = VecDeque::new();
        while self.cd_r(0) & 0x20 != 0 && resp.len() < 16 {
            resp.push_back(self.cd_r(1));
        }
        if flags & 7 != 0 {
            self.cd_w(0, 1);
            self.cd_w(3, 7);
        }
        if flags & 0x18 != 0 {
            self.cd_w(0, 1);
            self.cd_w(3, flags & 0x18);
        }
        let first = resp.pop_front().unwrap_or(0);
        match flags & 7 {
            1 => self.cd_data_ready(),
            2 => self.cd_complete(),
            3 => self.cd_acknowledge(first),
            4 => self.cd_end(),
            5 => self.cd_disc_error(first, resp.pop_front().unwrap_or(0)),
            _ => {}
        }
        1
    }

    /// A(91h) DMA's verifier: with IRQ3 taken, DMA3's flag acknowledged
    /// (whichever channel raised it, as OpenBIOS's does), a transfer
    /// counted, event 10h after the last; 1.
    pub fn cd_dma_verifier(&mut self) -> u32 {
        if self.io.is_none() || !self.irq_line(irq::DMA) {
            return 0;
        }
        let dicr = self.reg_read(dma::DICR, 4).unwrap_or(0);
        let _ = self.reg_write(dma::DICR, 4, dicr & 0xFF_FFFF | 0x8800_0000);
        self.kernel.cd.dmas -= 1;
        if self.kernel.cd.dmas == 0 {
            self.cd_event(ACK);
        }
        1
    }

    /// A(92h)/A(93h) the handlers: with auto-acknowledge on, the interrupt
    /// acknowledged and the exception returned from (the rest of the chains
    /// skipped).
    pub fn cd_handler(&mut self, dma: bool) -> bool {
        if self.kernel.cd.auto_ack[dma as usize] == 0 {
            return false;
        }
        let bit = if dma { irq::DMA } else { irq::CDROM };
        let _ = self.reg_write(irq::I_STAT, 4, !(1 << bit));
        true
    }

    fn cd_data_ready(&mut self) {
        let (p, st) = (self.kernel.cd.preempted, self.kernel.cd.state);
        if matches!(p, READN | READS) || !matches!(p, READN_PREEMPTED | READS_PREEMPTED) && matches!(st, READN | READS) {
            self.cd_initiate_dma();
        } else if matches!(p, READN_PREEMPTED | READS_PREEMPTED) || matches!(st, READN_PREEMPTED | READS_PREEMPTED) {
            self.cd_event(RDY);
        } else {
            self.cd_event(0x200);
        }
    }

    /// A sector into the buffer: the controller's data request, DMA3 over
    /// it; after the last, Pause.
    fn cd_initiate_dma(&mut self) {
        if self.kernel.cd.sectors == 0 {
            self.kernel.cd.sectors = -1;
        }
        self.cd_w(0, 0);
        self.cd_r(0);
        self.cd_w(3, 0);
        self.cd_r(3);
        self.cd_w(0, 0);
        self.cd_w(3, 0x80);
        let _ = self.reg_write(CDROM_DELAY, 4, 0x2_0943);
        let _ = self.reg_write(COM_DELAY, 4, 0x132C);
        self.kernel.cd.sectors -= 1;
        let (buf, words) = (self.kernel.cd.buf, self.kernel.cd.words);
        self.kernel.cd.buf = buf.wrapping_add(4 * words);
        let dicr = self.reg_read(dma::DICR, 4).unwrap_or(0);
        let _ = self.reg_write(dma::DICR, 4, dicr & 0xFF_FFFF | 0x88_0000);
        let dpcr = self.reg_read(dma::DPCR, 4).unwrap_or(0);
        let _ = self.reg_write(dma::DPCR, 4, dpcr | 0x8000);
        let _ = self.reg_write(D3_MADR, 4, buf);
        let _ = self.reg_write(D3_MADR + 4, 4, words | 0x1_0000);
        let _ = self.reg_write(D3_MADR + 8, 4, 0x1100_0000);
        if self.kernel.cd.sectors != 0 {
            return;
        }
        self.cd_command(0x09, &[]);
        self.kernel.cd.state = PAUSING;
    }

    fn cd_complete(&mut self) {
        let st = self.kernel.cd.state;
        let event = match st {
            INITIALIZING => {
                self.kernel.cd.init = 1;
                None
            }
            0x08 | 0x09 | SEEKL | SEEKP => {
                if matches!(self.kernel.cd.preempted, READN_PREEMPTED | READS_PREEMPTED | 3..=5) {
                    self.kernel.cd.preempted = IDLE;
                }
                Some(DNE)
            }
            REINIT => {
                self.kernel.cd.got_int5 = false;
                Some(ERR)
            }
            IDLE => Some(0x200),
            _ => Some(DNE),
        };
        self.kernel.cd.state = IDLE;
        if let Some(e) = event {
            self.cd_event(e);
        }
    }

    fn cd_acknowledge(&mut self, status: u8) {
        match self.kernel.cd.state {
            SETMODE | 0x17 => {
                self.kernel.cd.state = IDLE;
                self.cd_event(DNE);
            }
            GETSTATUS => {
                let dst = self.kernel.cd.status_dst;
                self.mem.w8(dst, status);
                self.kernel.cd.state = IDLE;
                self.cd_event(DNE);
            }
            READN | READS | READN_PREEMPTED | READS_PREEMPTED => self.kernel.cd.got_int3 = true,
            0xEE | READ_SETMODE => {
                let plain = self.kernel.cd.mode & 0x100 == 0;
                let first = self.kernel.cd.state == READ_SETMODE;
                self.kernel.cd.state = match (plain, first) {
                    (true, true) => READN,
                    (true, false) => READN_PREEMPTED,
                    (false, true) => READS,
                    (false, false) => READS_PREEMPTED,
                };
                self.cd_command(if plain { 0x06 } else { 0x1B }, &[]);
            }
            SEEKL_SETLOC => {
                self.kernel.cd.state = SEEKL;
                self.cd_command(SEEKL as u8, &[]);
            }
            IDLE => self.cd_event(0x200),
            _ => {}
        }
    }

    fn cd_end(&mut self) {
        let reading = |s: u32| matches!(s, READN | READS);
        let preempting = |s: u32| matches!(s, READN_PREEMPTED | READS_PREEMPTED);
        if reading(self.kernel.cd.preempted) || reading(self.kernel.cd.state) {
            if self.kernel.cd.dmas > 0 {
                self.cd_event(END);
            }
            if reading(self.kernel.cd.state) {
                self.kernel.cd.state = IDLE;
            } else {
                self.kernel.cd.preempted = IDLE;
            }
        }
        if preempting(self.kernel.cd.state) {
            self.kernel.cd.state = IDLE;
        } else if preempting(self.kernel.cd.preempted) {
            self.kernel.cd.preempted = IDLE;
        }
        self.cd_event(0x200);
    }

    fn cd_disc_error(&mut self, status: u8, error: u8) {
        self.kernel.cd.err = [status, error];
        if self.kernel.cd.state == INITIALIZING {
            self.kernel.cd.init = 2;
        } else if !self.kernel.cd.got_int5 {
            self.kernel.cd.got_int5 = true;
            self.kernel.cd.preempted = IDLE;
            self.kernel.cd.state = REINIT;
            self.cd_command(0x0A, &[]);
        } else {
            self.kernel.cd.got_int5 = false;
            self.kernel.cd.preempted = IDLE;
            self.kernel.cd.state = IDLE;
            self.cd_event(ERR);
        }
    }

    /// Wait for event 20h or one of `others` (test them in that order, as
    /// TestEvent does: the ready one goes busy again); `then` with which
    /// (0 for 20h, else 1 + its index in `others`).
    fn cd_wait(&mut self, others: &[u32], then: impl FnOnce(&mut Self, usize) -> Flow<S> + 'static) -> Flow<S> {
        let ev = |spec: u32| self.kernel.cd.events[SPECS.iter().position(|&s| s == spec).expect("a CD event")];
        let mut hs = [ev(DNE), 0, 0];
        for (i, &o) in others.iter().enumerate() {
            hs[i + 1] = ev(o);
        }
        Flow::Block {
            until: Wait::Events(hs),
            then: Box::new(move |m| {
                let i = hs.iter().position(|&h| h != 0 && m.event_ready(h)).expect("an event is ready");
                m.set_ev_status(hs[i], crate::kernel::EV_ENABLED);
                then(m, i)
            }),
        }
    }

    /// A(A5h) `CdReadSector(count, sector, dst)` (OpenBIOS
    /// `cdromBlockReading`): SeekL to the sector, then ReadN at double
    /// speed into `dst`; `count`, or a SystemError.
    pub fn cd_block_read(&mut self, count: u32, sector: u32, dst: u32, then: impl FnOnce(&mut Self, u32) -> Flow<S> + 'static) -> Result<Flow<S>, String> {
        self.cd_installed()?;
        let s = sector + cd::PREGAP;
        let msf = [bcd(s / 4500), bcd(s % 4500 / 75), bcd(s % 75)];
        if self.cd_seek_l(msf) == 0 {
            return Err("SystemError('D', 0xb): the BIOS's CD driver is busy".into());
        }
        Ok(self.cd_wait(&[ERR], move |m, i| {
            if i != 0 {
                system_error(0x0C, "the seek failed");
            }
            if m.cd_read(count, dst, 0x80) == 0 {
                system_error(0x0C, "the BIOS's CD driver is busy");
            }
            m.cd_wait(&[ERR, END], move |m, i| match i {
                0 => then(m, count),
                1 => system_error(0x16, "the read failed"),
                _ => system_error(0x17, "the read reached the end of the data"),
            })
        }))
    }

    /// A(A6h) `CdGetStatus()` (OpenBIOS `cdromBlockGetStatus`): the drive's
    /// status byte.
    pub fn cd_block_status(&mut self, then: impl FnOnce(&mut Self, u32) -> Flow<S> + 'static) -> Result<Flow<S>, String> {
        self.cd_installed()?;
        if self.cd_get_status(STATUS) == 0 {
            return Err("SystemError('D', 0x1f): the BIOS's CD driver is busy".into());
        }
        Ok(self.cd_wait(&[ERR], move |m, i| {
            if i != 0 {
                system_error(0x20, "GetStat failed");
            }
            let s = m.mem.r8(STATUS) as u32;
            then(m, s)
        }))
    }

    /// The primary volume descriptor, then the path table, into the
    /// device's own (OpenBIOS `cdromReadPathTable`); whether it went.
    fn cd_read_path_table(&mut self, then: impl FnOnce(&mut Self, bool) -> Flow<S> + 'static) -> Result<Flow<S>, String> {
        self.cd_block_read(1, 16, BUFFER, move |m, n| {
            let b = m.mem.bytes(BUFFER, cd::USER);
            if n != 1 || &b[1..6] != b"CD001" {
                return then(m, false);
            }
            m.kernel.cd.fs.hash = le(&b, 80);
            let lba = le(&b, 140);
            let r = m.cd_block_read(1, lba, BUFFER, move |m, n| {
                if n != 1 {
                    return then(m, false);
                }
                let b = m.mem.bytes(BUFFER, cd::USER);
                m.cd_path_table(&b);
                then(m, true)
            });
            r.unwrap_or_else(|e| panic!("{e}"))
        })
    }

    /// The path table from its sector: 45 entries at most, each one's
    /// parent the sum of its parent number's two bytes (the retail BIOS's).
    fn cd_path_table(&mut self, b: &[u8]) {
        let fs = &mut self.kernel.cd.fs;
        fs.hash = b.chunks(4).fold(fs.hash, |h, w| h ^ le(w, 0));
        fs.paths.clear();
        let mut p = 0;
        while p + 8 < b.len() && fs.paths.len() < 45 && b[p] != 0 {
            let n = b[p] as usize;
            let name = b[p + 8..(p + 8 + n).min(b.len())].to_vec();
            fs.paths.push((b[p + 6] as u32 + b[p + 7] as u32, le(b, p + 2), name));
            p += n + 8 + (n & 1);
        }
        fs.dir_id = 0;
    }

    /// The path table and PVD the boot read, through the host.
    pub fn cd_boot(&mut self) {
        let mut disc = crate::bios::HostDisc(&mut *self.host);
        let mut raw = [0u8; cd::RAW];
        if !cd::Disc::read_raw(&mut disc, 16, &mut raw) {
            return;
        }
        let pvd = cd::user_data(&raw).to_vec();
        if &pvd[1..6] != b"CD001" || !cd::Disc::read_raw(&mut disc, le(&pvd, 140), &mut raw) {
            return;
        }
        self.kernel.cd.fs.hash = le(&pvd, 80);
        let table = cd::user_data(&raw).to_vec();
        self.cd_path_table(&table);
    }

    /// Directory `id`'s first sector, unless it is the one read last
    /// (OpenBIOS `readDirectory`): 40 entries at most. A failed read keeps
    /// the last one's.
    fn cd_read_dir(&mut self, id: usize, then: impl FnOnce(&mut Self) -> Flow<S> + 'static) -> Result<Flow<S>, String> {
        if self.kernel.cd.fs.dir_id == id {
            return Ok(then(self));
        }
        let lba = self.kernel.cd.fs.paths.get(id.wrapping_sub(1)).map_or(0, |p| p.1);
        self.cd_block_read(1, lba, BUFFER, move |m, n| {
            if n == 1 {
                let b = m.mem.bytes(BUFFER, cd::USER);
                let mut dir = Vec::new();
                let mut p = 0;
                while p + 33 < b.len() && dir.len() < 40 && b[p] != 0 {
                    let n = b[p + 32] as usize;
                    dir.push((le(&b, p + 2), le(&b, p + 10), b[p + 33..(p + 33 + n).min(b.len())].to_vec()));
                    p += b[p] as usize;
                }
                m.kernel.cd.fs.dir = dir;
                m.kernel.cd.fs.dir_id = id;
            }
            then(m)
        })
    }

    /// The entry `name` matches, from entry `from` of its directory
    /// (OpenBIOS `findDirectoryEntryForFilename`): the current directory
    /// unless it starts with `\`, its directories through the path table
    /// (8 deep at most).
    fn cd_find(&mut self, from: usize, name: Vec<u8>, then: impl FnOnce(&mut Self, Option<usize>) -> Flow<S> + 'static) -> Result<Flow<S>, String> {
        let mut full = name.clone();
        if name.first() != Some(&b'\\') {
            full = self.kernel.cd.fs.cwd.clone();
            full.push(b'\\');
            full.extend(&name);
        }
        let parts: Vec<&[u8]> = full[1..].split(|&c| c == b'\\').collect();
        let (file, dirs) = parts.split_last().expect("split yields one");
        let mut id = 1;
        for (depth, d) in dirs.iter().enumerate() {
            let found = self.kernel.cd.fs.paths.iter().position(|p| p.0 == id as u32 && p.2 == *d);
            match found {
                Some(i) if depth < 7 => id = i + 1,
                _ => return Ok(then(self, None)),
            }
        }
        let file = file.to_vec();
        self.cd_read_dir(id, move |m| {
            let hit = m.kernel.cd.fs.dir.iter().enumerate().skip(from).find(|(_, e)| matches(&e.2, &file)).map(|(i, _)| i);
            if let Some(i) = hit {
                m.kernel.cd.fs.found = i;
            }
            then(m, hit)
        })
    }

    /// The drive's status, and the path table again when the shell was
    /// opened (bit 4); `then` unless that read failed.
    fn cd_check(&mut self, f: u32, then: impl FnOnce(&mut Self) -> Flow<S> + 'static) -> Result<Flow<S>, String> {
        self.cd_block_status(move |m, st| {
            if st & 0x10 == 0 {
                return then(m);
            }
            let r = m.cd_read_path_table(move |m, ok| {
                if !ok {
                    m.mem.w32(f + fcb::ERRNO, files::E_GENERAL);
                    return Flow::ret(u32::MAX);
                }
                then(m)
            });
            r.unwrap_or_else(|e| panic!("{e}"))
        })
    }

    /// A(5Fh) `dev_cd_open(fcb, name, mode)`: the name upper-cased, `;1`
    /// added without a version; the file's LBA, size and the disc's hash
    /// in the FCB; 0, or -1.
    pub fn dev_cd_open(&mut self, f: u32, name: u32) -> Result<Flow<S>, String> {
        let mut n: Vec<u8> = self.mem.cstr(name, 128).to_ascii_uppercase();
        if !n.contains(&b';') {
            n.extend(b";1");
        }
        self.cd_check(f, move |m| {
            let r = m.cd_find(0, n, move |m, hit| {
                let Some(i) = hit else {
                    m.mem.w32(f + fcb::ERRNO, files::E_NOT_FOUND);
                    return Flow::ret(u32::MAX);
                };
                let (lba, size, _) = m.kernel.cd.fs.dir[i];
                let hash = m.kernel.cd.fs.hash;
                for (o, v) in [(fcb::LBA, lba), (fcb::LENGTH, size), (fcb::OFFSET, 0), (fcb::ERRNO, 0), (fcb::DEVICE_ID, hash)] {
                    m.mem.w32(f + o, v);
                }
                Flow::ret(0)
            });
            r.unwrap_or_else(|e| panic!("{e}"))
        })
    }

    /// A(60h) `dev_cd_read(fcb, dst, len)`: whole sectors from a sector
    /// boundary, the path table reloaded and the disc checked first; the
    /// bytes up to the file's end, or -1.
    pub fn dev_cd_read(&mut self, f: u32, dst: u32, len: u32) -> Result<Flow<S>, String> {
        let (off, size) = (self.mem.r32(f + fcb::OFFSET), self.mem.r32(f + fcb::LENGTH));
        if len & 0x7FF != 0 || off & 0x7FF != 0 || (len as i32) < 0 || off >= size {
            self.mem.w32(f + fcb::ERRNO, files::E_ALIGN);
            return Ok(Flow::ret(u32::MAX));
        }
        let busy = move |m: &mut Self| {
            m.mem.w32(f + fcb::ERRNO, files::E_GENERAL);
            Flow::ret(u32::MAX)
        };
        let n = len >> 11;
        self.cd_block_status(move |m, st| {
            if st & 0x10 != 0 {
                return busy(m);
            }
            let r = m.cd_read_path_table(move |m, ok| {
                if !ok || m.kernel.cd.fs.hash != m.mem.r32(f + fcb::DEVICE_ID) {
                    return busy(m);
                }
                let lba = m.mem.r32(f + fcb::LBA) + (off >> 11);
                let r = m.cd_block_read(n, lba, dst, move |m, got| {
                    if got != n {
                        return busy(m);
                    }
                    let size = (n << 11).min(size - off);
                    m.mem.w32(f + fcb::OFFSET, off + size);
                    Flow::ret(size)
                });
                r.unwrap_or_else(|e| panic!("{e}"))
            });
            r.unwrap_or_else(|e| panic!("{e}"))
        })
    }

    /// A(62h) `dev_cd_firstfile(fcb, pattern, direntry)`: the path table
    /// reloaded; the pattern upper-cased, `*` (and nothing) as `?`s.
    pub fn dev_cd_firstfile(&mut self, f: u32, name: u32, dir: u32) -> Result<Flow<S>, String> {
        let name = self.mem.cstr(name, 128);
        let mut pat: Vec<u8> = name.iter().take_while(|&&c| c != b'*').map(u8::to_ascii_uppercase).collect();
        pat.truncate(20);
        if name.is_empty() || name.contains(&b'*') {
            pat.resize(20, b'?');
        }
        self.cd_read_path_table(move |m, ok| {
            if !ok {
                return Flow::ret(0);
            }
            let fs = &mut m.kernel.cd.fs;
            fs.pattern = pat;
            fs.found = 0;
            fs.first_skip = true;
            let r = m.dev_cd_nextfile(f, dir).map(|flow| {
                seq(m, flow, |m, v| {
                    m.kernel.cd.fs.first_dir = m.kernel.cd.fs.dir_id;
                    Flow::Return(v)
                })
            });
            r.unwrap_or_else(|e| panic!("{e}"))
        })
    }

    /// A(63h) `dev_cd_nextfile(fcb, direntry)`: the next entry the pattern
    /// matches, into the direntry; the direntry, or 0.
    pub fn dev_cd_nextfile(&mut self, f: u32, dir: u32) -> Result<Flow<S>, String> {
        let _ = f;
        self.cd_block_status(move |m, st| {
            let go = move |m: &mut Self| {
                let fs = &m.kernel.cd.fs;
                if !fs.first_skip && fs.dir_id != fs.first_dir {
                    return Flow::ret(0);
                }
                m.kernel.cd.fs.first_skip = false;
                let (from, pat) = (m.kernel.cd.fs.found + 1, m.kernel.cd.fs.pattern.clone());
                let r = m.cd_find(from, pat, move |m, hit| {
                    let Some(i) = hit else { return Flow::ret(0) };
                    let (lba, size, mut name) = m.kernel.cd.fs.dir[i].clone();
                    name.push(0);
                    m.mem.write(dir, &name);
                    for (o, v) in [(0x14, 0), (0x18, size), (0x20, lba)] {
                        m.mem.w32(dir + o, v);
                    }
                    Flow::ret(dir)
                });
                r.unwrap_or_else(|e| panic!("{e}"))
            };
            if st & 0x10 == 0 {
                return go(m);
            }
            let old = m.kernel.cd.fs.hash;
            let r = m.cd_read_path_table(move |m, ok| {
                if !ok || m.kernel.cd.fs.hash != old {
                    m.kernel.cd.fs.hash = old;
                    return Flow::ret(0);
                }
                go(m)
            });
            r.unwrap_or_else(|e| panic!("{e}"))
        })
    }

    /// A(64h) `dev_cd_chdir(fcb, path)`: the current directory, upper-cased,
    /// and the path table reloaded; 1, or 0.
    pub fn dev_cd_chdir(&mut self, path: u32) -> Result<Flow<S>, String> {
        self.kernel.cd.fs.cwd = self.mem.cstr(path, 128).to_ascii_uppercase();
        self.cd_read_path_table(|_, ok| Flow::ret(ok as u32))
    }

    /// A(A4h) `CdGetLbn(name)`: the file's first sector; psx-spx's bug: -1
    /// for a directory's first entry. -1 when not found (psx-spx: garbage).
    pub fn cd_get_lbn(&mut self, name: u32) -> Result<Flow<S>, String> {
        let mut n: Vec<u8> = self.mem.cstr(name, 128).to_ascii_uppercase();
        if let Some(c) = n.iter().position(|&c| c == b':') {
            n.drain(..=c);
        }
        if !n.contains(&b';') {
            n.extend(b";1");
        }
        self.cd_find(0, n, |m, hit| match hit {
            Some(i) if i > 0 => Flow::ret(m.kernel.cd.fs.dir[i].0),
            _ => Flow::ret(u32::MAX),
        })
    }

    /// A(54h)/A(71h) `_96_init()` (OpenBIOS `initCDRom`): installed, Init
    /// issued and its completion waited for, then the path table read.
    pub fn cd_init(&mut self) -> Result<Flow<S>, String> {
        self.cd_install();
        self.cd_installed()?;
        self.kernel.cd.state = INITIALIZING;
        self.kernel.cd.init = 0;
        self.cd_command(0x0A, &[]);
        let deadline = self.clock.now + psx_devices::clock::CPU_HZ;
        Ok(self.cd_init_wait(deadline))
    }

    /// `cdromInnerInit`'s wait for Init's completion, a millisecond at a
    /// time.
    fn cd_init_wait(&mut self, deadline: u64) -> Flow<S> {
        Flow::Block {
            until: Wait::Cycle(self.clock.now + psx_devices::clock::CPU_HZ / 1000),
            then: Box::new(move |m| match m.kernel.cd.init {
                1 => m.cd_read_path_table(|_, _| Flow::ret(0)).unwrap_or_else(|e| panic!("{e}")),
                0 if m.clock.now < deadline => m.cd_init_wait(deadline),
                _ => system_error(0x0A, "the drive didn't initialise"),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Flow;
    use crate::testing::{NoSdk, machine};
    use psx_devices::cd;

    /// A mode 2 form 1 disc: PVD at 16, path table at 17, root at 18 with
    /// FILE.BIN;1 (2 sectors), directory DATA at 19 with X.BIN;1.
    pub fn disc() -> Vec<[u8; cd::RAW]> {
        let sector = |lba: u32, data: &[u8]| {
            let mut s = [0u8; cd::RAW];
            s[..12].copy_from_slice(&[0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0]);
            s[12..15].copy_from_slice(&cd::lba_to_msf(lba));
            s[15] = 2;
            s[0x18..0x18 + data.len()].copy_from_slice(data);
            let e = cd::edc(&s[0x10..0x818]);
            s[0x818..0x81C].copy_from_slice(&e.to_le_bytes());
            s
        };
        let rec = |name: &[u8], lba: u32, size: u32| {
            let mut r = vec![0u8; 33 + name.len() + (name.len() + 1) % 2];
            r[0] = r.len() as u8;
            r[2..6].copy_from_slice(&lba.to_le_bytes());
            r[10..14].copy_from_slice(&size.to_le_bytes());
            r[32] = name.len() as u8;
            r[33..33 + name.len()].copy_from_slice(name);
            r
        };
        let mut s: Vec<[u8; cd::RAW]> = (0..24).map(|l| sector(l, &[])).collect();
        let mut pvd = vec![0u8; cd::USER];
        pvd[..6].copy_from_slice(b"\x01CD001");
        pvd[80..84].copy_from_slice(&24u32.to_le_bytes());
        pvd[140..144].copy_from_slice(&17u32.to_le_bytes());
        s[16] = sector(16, &pvd);
        let mut pt = vec![1, 0, 18, 0, 0, 0, 1, 0, 0, 0];
        pt.extend([4, 0, 19, 0, 0, 0, 1, 0]);
        pt.extend(b"DATA");
        s[17] = sector(17, &pt);
        let mut root = rec(b"\0", 18, 2048);
        root.extend(rec(b"\x01", 18, 2048));
        root.extend(rec(b"FILE.BIN;1", 20, 3000));
        s[18] = sector(18, &root);
        let mut sub = rec(b"\0", 19, 2048);
        sub.extend(rec(b"\x01", 18, 2048));
        sub.extend(rec(b"X.BIN;1", 22, 5));
        s[19] = sector(19, &sub);
        s[20] = sector(20, &[0xAB; cd::USER]);
        s[21] = sector(21, &[0xCD; cd::USER]);
        s[22] = sector(22, b"hello");
        s
    }

    /// Run a flow as the runtime's driver does: time passes event by
    /// event, the exception handler's kernel steps run.
    pub fn run(m: &mut crate::Machine<NoSdk>, mut flow: Flow<NoSdk>) -> u32 {
        loop {
            match flow {
                Flow::Return(v) => return v as u32,
                Flow::Call { .. } => panic!("no guest here"),
                Flow::Block { until, then } => {
                    let mut n = 0;
                    loop {
                        while let Some(s) = m.irq_calls.pop_front() {
                            if let crate::IrqStep::Kernel(k) = s {
                                if m.kernel_step(k) {
                                    while m.irq_calls.pop_front().is_some_and(|s| s != crate::IrqStep::Exit) {}
                                }
                            }
                        }
                        if m.holds(until) {
                            break;
                        }
                        let limit = m.clock.next_time().unwrap_or(m.clock.now + 1000);
                        m.step(limit);
                        n += 1;
                        assert!(n < 100_000, "never ends: {until:?}");
                    }
                    flow = then(m);
                }
            }
        }
    }

    pub fn booted() -> crate::Machine<NoSdk> {
        let mut m = crate::testing::machine_with_disc(disc());
        m.io = Some(Default::default());
        m.cdc_insert();
        m.cd_install();
        m.cd_boot();
        m
    }

    /// Files on the disc through the device table: open, read with the
    /// drive's time passing, the second directory, firstfile/nextfile.
    #[test]
    fn cdrom_files() {
        let mut m = booted();
        let s = 0x8001_0000;
        m.mem.write(s, b"cdrom:\\FILE.BIN;1\0");
        let f = m.open(s, 1).unwrap();
        assert_eq!(run(&mut m, f), 2);
        let t = m.clock.now;
        let f = m.transfer(2, 0x8002_0000, 0x1000, false).unwrap();
        assert_eq!(run(&mut m, f), 3000);
        assert!(m.clock.now > t + 1000, "the drive's time");
        assert_eq!((m.mem.r8(0x8002_0000), m.mem.r8(0x8002_0FFF)), (0xAB, 0xCD));
        let f = m.transfer(2, 0x8002_0000, 0x800, false).unwrap();
        assert_eq!(run(&mut m, f), u32::MAX, "past the end");
        assert_eq!(m.kernel.files.errno, crate::files::E_ALIGN);
        m.mem.write(s, b"cdrom:\\DATA\\X.BIN\0");
        let f = m.open(s, 1).unwrap();
        assert_eq!(run(&mut m, f), 3);
        m.mem.write(s, b"cdrom:\\NOPE.BIN\0");
        let f = m.open(s, 1).unwrap();
        assert_eq!(run(&mut m, f), u32::MAX);
        assert_eq!(m.kernel.files.errno, crate::files::E_NOT_FOUND);
        m.mem.write(s, b"cdrom:F*\0");
        let f = m.firstfile(s, 0x8003_0000).unwrap();
        assert_eq!(run(&mut m, f), 0x8003_0000);
        assert_eq!(m.mem.cstr(0x8003_0000, 20), b"FILE.BIN;1");
        assert_eq!(m.mem.r32(0x8003_0020), 20);
        let f = m.nextfile(0x8003_0000).unwrap();
        assert_eq!(run(&mut m, f), 0);
        // CdGetLbn, CdReadSector.
        m.mem.write(s, b"\\DATA\\X.BIN;1\0");
        let f = m.cd_get_lbn(s).unwrap();
        assert_eq!(run(&mut m, f), 22);
        let f = m.cd_block_read(1, 22, 0x8004_0000, |_, n| Flow::ret(n)).unwrap();
        assert_eq!(run(&mut m, f), 1);
        assert_eq!(m.mem.bytes(0x8004_0000, 5), b"hello");
        // _96_remove: the driver is out of its chain, its events closed.
        crate::bios::call(&mut m, 0xA0, 0x72, [0; 4]).unwrap();
        assert!(m.chain(0).iter().all(|&e| m.kernel_element_at(e) == Some(crate::kernel::SYSCALL_ELEMENT)));
        assert!(m.cd_block_read(1, 22, 0x8004_0000, |_, n| Flow::ret(n)).is_err());
        let _ = machine;
    }
}
