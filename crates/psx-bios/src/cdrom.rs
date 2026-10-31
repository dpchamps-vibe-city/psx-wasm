//! The BIOS's CD-ROM software services (psx-spx "BIOS CDROM Functions";
//! OpenBIOS `cdrom/`) over the native drive and disc, not the controller's
//! registers.
//!
//! The retail functions are a driver state machine: `A(90h..93h)` read the
//! controller's interrupt and response, issue the next command, start DMA3
//! and deliver the CD events (class F0000003h: 20h a command done, 40h data
//! ready, 80h end of data, 8000h an error). That register protocol is not
//! reproduced here. The software calls keep their signatures, guest
//! buffers, results and event order, and drive [`psx_devices::cd::Drive`]
//! and the host's disc directly on the virtual clock; the sector bytes are
//! the same ones the fallback controller reads through its own core.
//!
//! `_96_init` opens the five events; `_96_remove` closes them. The async
//! calls return at once and schedule their completion; a blocking call
//! waits for the same completion event. A read's last buffer reports ACK
//! (10h), then its normal pause reports DNE (20h); END (80h) is only the
//! drive ending early. The fallback controller's own state is updated too,
//! so later core queries see the head, mode, subchannel/header and status.
//! Only one operation runs at a time, as the retail driver's state is idle
//! before the next command.

use crate::kernel::{self, EV_MODE_NOINTR};
use crate::machine::{Event, Flow, GuestCall, Machine, Sdk, Wait};
use psx_devices::cd::{self, Drive};
use std::{cell::Cell, rc::Rc};

pub const EV_CD: u32 = 0xF000_0003;
pub const ACK: u32 = 0x10;
pub const DNE: u32 = 0x20;
pub const RDY: u32 = 0x40;
pub const END: u32 = 0x80;
pub const ERR: u32 = 0x8000;
const SPECS: [u32; 5] = [ACK, DNE, RDY, END, ERR];

/// A command's sector-count argument is signed (0 and negative fail).
#[derive(Debug, Clone)]
enum Op {
    Seek {
        at: u64,
    },
    Status {
        dst: Option<u32>,
        at: u64,
    },
    SetMode {
        mode: u8,
        at: u64,
    },
    Read {
        lba: u32,
        left: u32,
        dst: u32,
        mode: u8,
        at: u64,
    },
    /// The pause after the last sector's buffer is complete: the read's
    /// normal DNE follows the DMA/read ACK.
    Pause {
        at: u64,
    },
}

impl Op {
    fn at(&self) -> u64 {
        match self {
            Op::Seek { at }
            | Op::Status { at, .. }
            | Op::SetMode { at, .. }
            | Op::Read { at, .. }
            | Op::Pause { at } => *at,
        }
    }
}

/// The BIOS CD driver's software state: its events, the operation in
/// flight, and the last completion or INT5 error.
#[derive(Debug, Clone, Default)]
pub struct Cd {
    pub events: [u32; 5],
    op: Option<Op>,
    /// Each blocking waiter retains its own result even if a completion
    /// callback immediately starts another operation.
    done: Rc<Cell<Option<u64>>>,
    /// The event spec of the last operation that finished.
    pub last: Option<u32>,
    /// The last status byte read by a status operation (`CdGetStatus`).
    pub last_status: u8,
    /// The last controller error's status and code
    /// (`CdromGetInt5errCode`). The native read path has no command INT5
    /// stage: the core records EDC failures in its `edc_errors`, and a
    /// missing sector is an early end, so this stays zero on that path.
    pub error: [u8; 2],
}

fn bcd(v: u32) -> u8 {
    (((v / 10) << 4) | (v % 10)) as u8
}

/// Call A(A1h), returning -1 if a guest table patch returns. A read error
/// can instead sequence another attempt after this flow (OpenBIOS helpers.c).
/// The default SystemError is fatal; retain the callable boundary.
fn system_error<S: Sdk>(m: &Machine<S>, code: u32) -> Flow<S> {
    Flow::Call {
        call: GuestCall {
            addr: m.mem.r32(crate::tables::entry(0xA0, 0xA1).unwrap()),
            args: [b'D' as u32, code, 0, 0],
        },
        then: Box::new(|_, _| Flow::ret(u32::MAX)),
    }
}

/// The bytes one read sector puts in the guest buffer for `mode`: 2048
/// user bytes, 2340 after the sync, or 2328 after the sync and header.
fn sector_bytes(raw: &[u8; cd::RAW], mode: u8) -> &[u8] {
    if mode & 0x10 != 0 {
        &raw[24..]
    } else if mode & 0x20 != 0 {
        &raw[12..]
    } else {
        cd::user_data(raw)
    }
}

/// Wait for this operation, not one started by its completion callback.
/// Low word: completion spec; high word: captured status.
fn cd_wait<S: Sdk>(m: &mut Machine<S>, done: Rc<Cell<Option<u64>>>) -> Flow<S> {
    match done.get() {
        Some(result) => Flow::Return(result),
        None => Flow::Block {
            until: Wait::Cycle(m.cd_due().max(m.clock.now + 1)),
            then: Box::new(move |m| cd_wait(m, done)),
        },
    }
}

impl<S: Sdk> Machine<S> {
    /// The drive the BIOS's CD calls act on: the fallback controller's own
    /// when the bus is there, so both see one head, mode and status, else
    /// the machine's native drive.
    pub fn cd_drive(&mut self) -> &mut Drive {
        match self.io.as_mut() {
            Some(io) => &mut io.cdc.drive,
            None => &mut self.drive,
        }
    }

    /// The drive status as the fallback controller reports it, or the
    /// native drive's byte without a bus.
    pub fn cd_status(&self) -> u8 {
        match &self.io {
            Some(io) => io.cdc.status(),
            None => self.drive.status,
        }
    }

    fn cd_owns(&mut self, index: usize, h: u32) -> bool {
        h != 0
            && self.ev(h).is_some_and(|e| {
                e.class == EV_CD
                    && e.spec == SPECS[index]
                    && e.mode == EV_MODE_NOINTR
                    && e.status != kernel::EV_UNUSED
            })
    }

    /// Whether `_96_init`'s five events are ours and still open.
    pub fn cd_installed(&mut self) -> bool {
        let events = self.kernel.cd.events;
        (0..SPECS.len()).all(|i| self.cd_owns(i, events[i]))
    }

    /// A(54h)/A(71h) `_96_init`: all five events
    /// open and enabled, or none left behind and a failure result.
    pub fn cd_init(&mut self) -> u32 {
        self.cd_remove();
        let mut events = [0u32; 5];
        for (i, spec) in SPECS.into_iter().enumerate() {
            let h = self.open_event(EV_CD, spec, EV_MODE_NOINTR, 0);
            if h == u32::MAX {
                for (j, &h) in events.iter().enumerate() {
                    if self.cd_owns(j, h) {
                        self.set_ev_status(h, kernel::EV_UNUSED);
                    }
                }
                self.kernel.cd.events = [0; 5];
                return u32::MAX;
            }
            self.set_ev_status(h, kernel::EV_ENABLED);
            events[i] = h;
        }
        self.kernel.cd.events = events;
        self.kernel.files.cd_cwd.clear();
        self.cd_inner_init();
        0
    }

    /// A(95h) `CdInitSubFunc`: reset the drive, not its event allocation or
    /// filesystem. The inner init returns 1 on success (the outer init retries 0).
    pub fn cd_inner_init(&mut self) -> u32 {
        self.cd_cancel();
        self.kernel.cd.last = None;
        self.kernel.cd.error = [0; 2];
        self.cdc_sync();
        if let Some(io) = self.io.as_mut() {
            io.cdc.native_init();
        } else {
            self.drive.status = cd::stat::STANDBY;
            self.drive.mode = 0;
        }
        self.cdc_sync();
        // Both the outer setup and the inner init leave the critical section.
        let _ = crate::bios::syscall(self, [2, 0, 0, 0]);
        1
    }

    /// A(56h)/A(72h) `_96_remove`: pending native work stopped, only our
    /// own events closed. A slot the guest reused for another event is left
    /// alone.
    pub fn cd_remove(&mut self) {
        self.crit = true;
        self.cd_cancel();
        let events = std::mem::take(&mut self.kernel.cd.events);
        for (i, h) in events.into_iter().enumerate() {
            if self.cd_owns(i, h) {
                self.set_ev_status(h, kernel::EV_UNUSED);
            }
        }
    }

    fn cd_cancel(&mut self) {
        self.clock.cancel(|e| *e == Event::Cd);
        if self.kernel.cd.op.take().is_some() {
            self.kernel.cd.done.set(Some(ERR as u64));
        }
        self.cd_native_finish();
    }

    fn cd_native_seek(&mut self, target: u32) -> u64 {
        self.cdc_sync();
        let cycles = if let Some(io) = self.io.as_mut() {
            io.cdc.native_seek(target)
        } else {
            let d = &mut self.drive;
            let cycles = d.seek_cycles(d.head, target);
            d.head = target;
            d.setloc = target;
            d.status = (d.status & !cd::stat::ERROR) | cd::stat::SEEK;
            cycles
        };
        self.cdc_sync();
        cycles
    }

    fn cd_native_seek_done(&mut self) {
        match self.io.as_mut() {
            Some(io) => io.cdc.native_seek_done(),
            None => self.drive.status &= !cd::stat::SEEK,
        }
    }

    fn cd_native_begin_read(&mut self, mode: u8) -> u64 {
        self.cdc_sync();
        let cycles = if let Some(io) = self.io.as_mut() {
            io.cdc.native_begin_read(mode)
        } else {
            let d = &mut self.drive;
            d.mode = mode;
            d.status = (d.status & !cd::stat::ERROR) | cd::stat::READ;
            d.sector_cycles()
        };
        self.cdc_sync();
        cycles
    }

    fn cd_native_finish(&mut self) {
        self.cdc_sync();
        match self.io.as_mut() {
            Some(io) => io.cdc.native_finish(),
            None => self.drive.status &= !(cd::stat::SEEK | cd::stat::READ),
        }
        self.cdc_sync();
    }

    fn cd_native_set_mode(&mut self, mode: u8) {
        match self.io.as_mut() {
            Some(io) => io.cdc.native_set_mode(mode),
            None => self.drive.mode = mode,
        }
    }

    /// One native read sector through the same disc/EDC validation and
    /// header/subchannel state the fallback controller uses.
    fn cd_native_sector(&mut self, lba: u32) -> Option<[u8; cd::RAW]> {
        let Machine {
            io, host, drive, ..
        } = self;
        if let Some(io) = io {
            let mut disc = crate::bios::HostDisc(&mut **host);
            io.cdc.native_sector(lba, &mut disc)
        } else {
            let mut raw = [0u8; cd::RAW];
            if !host.disc_read(lba, &mut raw) {
                return None;
            }
            let _ = cd::check(&raw, lba);
            drive.head = lba + 1;
            Some(raw)
        }
    }

    /// Start an operation at its due cycle. Stale ready events from the
    /// previous operation must not satisfy this one's wait.
    fn cd_begin(&mut self, op: Op) {
        self.clock.cancel(|e| *e == Event::Cd);
        for spec in SPECS {
            self.undeliver_event(EV_CD, spec);
        }
        self.kernel.cd.last = None;
        self.kernel.cd.done = Rc::new(Cell::new(None));
        let at = op.at();
        self.kernel.cd.op = Some(op);
        self.clock.schedule(at, Event::Cd);
    }

    /// The cycle the operation in flight completes at.
    pub fn cd_due(&self) -> u64 {
        self.kernel
            .cd
            .op
            .as_ref()
            .map_or(self.clock.now + 1, Op::at)
    }

    fn cd_finish(&mut self, spec: u32) {
        self.kernel.cd.op = None;
        self.kernel.cd.last = Some(spec);
        self.kernel.cd.done.set(Some(
            (self.kernel.cd.last_status as u64) << 32 | spec as u64,
        ));
        self.deliver_event(EV_CD, spec);
    }

    /// A blocking call consumes the event it waited for, as the retail
    /// driver's event wait does; only _96_init's own events are touched.
    fn cd_take(&mut self, spec: u32, done: &Rc<Cell<Option<u64>>>) {
        // A callback may already have completed a different operation.
        if !Rc::ptr_eq(done, &self.kernel.cd.done) {
            return;
        }
        let events = self.kernel.cd.events;
        for (i, h) in events.into_iter().enumerate() {
            if self.cd_owns(i, h)
                && self
                    .ev(h)
                    .is_some_and(|e| e.spec == spec && e.status == kernel::EV_READY)
            {
                self.set_ev_status(h, kernel::EV_ENABLED);
                return;
            }
        }
    }

    /// A(78h) `CdAsyncSeekL(msf)`: 1, or 0 when busy.
    pub fn cd_seek_l(&mut self, msf: [u8; 3]) -> u32 {
        if !self.cd_installed() || self.kernel.cd.op.is_some() {
            return 0;
        }
        let target = cd::msf_to_lba(msf);
        let now = self.clock.now;
        let cycles = self.cd_native_seek(target);
        self.cd_begin(Op::Seek { at: now + cycles });
        1
    }

    fn cd_get_status_at(&mut self, dst: Option<u32>) -> u32 {
        if !self.cd_installed() || self.kernel.cd.op.is_some() {
            return 0;
        }
        self.cdc_sync();
        let at = self.clock.now + self.cd_drive().ack_cycles();
        self.cd_begin(Op::Status { dst, at });
        1
    }

    /// A(7Ch) `CdAsyncGetStatus(dst)`: GetStat, its status byte to `dst`.
    pub fn cd_get_status(&mut self, dst: u32) -> u32 {
        self.cd_get_status_at(Some(dst))
    }

    /// A(7Eh) `CdAsyncReadSector(count, dst, mode)`: sectors from the
    /// head, each after its own time, into `dst`; 1, or 0 when busy.
    pub fn cd_read(&mut self, count: u32, dst: u32, mode: u32) -> u32 {
        if !self.cd_installed() || self.kernel.cd.op.is_some() || (count as i32) <= 0 {
            return 0;
        }
        let now = self.clock.now;
        let cycles = self.cd_native_begin_read(mode as u8);
        let lba = self.cd_drive().head;
        self.cd_begin(Op::Read {
            lba,
            left: count,
            dst,
            mode: mode as u8,
            at: now + cycles,
        });
        1
    }

    /// A(81h) `CdAsyncSetMode(mode)`.
    pub fn cd_set_mode(&mut self, mode: u32) -> u32 {
        if !self.cd_installed() || self.kernel.cd.op.is_some() {
            return 0;
        }
        self.cdc_sync();
        self.cd_native_set_mode(mode as u8);
        let at = self.clock.now + self.cd_drive().ack_cycles();
        self.cd_begin(Op::SetMode {
            mode: mode as u8,
            at,
        });
        1
    }

    /// A(94h) `CdromGetInt5errCode(dst1, dst2)`: the last error's two bytes.
    pub fn cd_int5_error(&mut self, dst1: u32, dst2: u32) {
        let [a, b] = self.kernel.cd.error;
        self.mem.w8(dst1, a);
        self.mem.w8(dst2, b);
    }

    /// One step of the operation in flight, at its due cycle.
    pub fn cd_step(&mut self) {
        let Some(op) = self.kernel.cd.op.clone() else {
            return;
        };
        let now = self.clock.now;
        if now < op.at() {
            self.clock.schedule(op.at(), Event::Cd);
            return;
        }
        match op {
            Op::Seek { .. } => {
                self.cd_native_seek_done();
                self.cd_finish(DNE);
            }
            Op::Status { dst, .. } => {
                let status = self.cd_status();
                self.kernel.cd.last_status = status;
                if let Some(dst) = dst {
                    self.mem.w8(dst, status);
                }
                self.cd_finish(DNE);
            }
            Op::SetMode { mode, .. } => {
                self.cd_native_set_mode(mode);
                self.cd_finish(DNE);
            }
            Op::Read {
                lba,
                left,
                dst,
                mode,
                ..
            } => {
                // A missing sector is the drive's early data end (80h), not
                // an invented controller INT5. The failed sector leaves the
                // guest buffer untouched.
                let Some(raw) = self.cd_native_sector(lba) else {
                    self.cd_native_finish();
                    self.cd_finish(END);
                    return;
                };
                let bytes = sector_bytes(&raw, mode);
                self.mem.write(dst, bytes);
                self.loaded.push((dst, bytes.len() as u32));
                if left <= 1 {
                    // The last buffer is complete: the core's DMA path
                    // reports ACK, then the normal pause completion DNE.
                    let at = now + self.cd_drive().ack_cycles();
                    self.kernel.cd.last = Some(ACK);
                    self.kernel.cd.op = Some(Op::Pause { at });
                    self.clock.cancel(|e| *e == Event::Cd);
                    self.clock.schedule(at, Event::Cd);
                    self.deliver_event(EV_CD, ACK);
                } else {
                    let cycles = self.cd_drive().sector_cycles();
                    let dst = dst.wrapping_add(bytes.len() as u32);
                    self.kernel.cd.op = Some(Op::Read {
                        lba: lba + 1,
                        left: left - 1,
                        dst,
                        mode,
                        at: now + cycles,
                    });
                    self.clock.schedule(now + cycles, Event::Cd);
                }
            }
            Op::Pause { .. } => {
                self.cd_native_finish();
                self.cd_finish(DNE);
            }
        }
    }

    /// A(A5h) `CdReadSector(count, sector, dst)` (OpenBIOS
    /// `cdromBlockReading`): SeekL to `sector`, then ReadN at double speed;
    /// `count`, or -1 on failure if SystemError returns. A read ERR retries
    /// after that call, up to ten attempts. Waits through every sector and the pause.
    pub fn cd_block_read(&mut self, count: u32, sector: u32, dst: u32) -> Result<Flow<S>, String> {
        Ok(self.cd_block_read_attempt(count, sector.wrapping_add(cd::PREGAP), dst, 10))
    }

    fn cd_block_read_attempt(&mut self, count: u32, sector: u32, dst: u32, left: u32) -> Flow<S> {
        if left == 0 {
            return system_error(self, 0x0C);
        }
        let msf = [bcd(sector / 4500), bcd(sector % 4500 / 75), bcd(sector % 75)];
        // Retail computes MSF inside the retry loop and loses the minutes
        // from `sector`. OpenBIOS helpers.c documents but fixes that bug.
        let sector = sector % 4500;
        if self.cd_seek_l(msf) == 0 {
            return system_error(self, 0x0B);
        }
        let done = self.kernel.cd.done.clone();
        let wait = cd_wait(self, done.clone());
        crate::machine::seq(self, wait, move |m, result| {
            if result as u32 != DNE {
                m.cd_take(ERR, &done);
                return system_error(m, 0x0C);
            }
            m.cd_take(DNE, &done);
            if m.cd_read(count, dst, 0x80) == 0 {
                return system_error(m, 0x0C);
            }
            let done = m.kernel.cd.done.clone();
            let wait = cd_wait(m, done.clone());
            crate::machine::seq(m, wait, move |m, result| match result as u32 {
                DNE => {
                    m.cd_take(DNE, &done);
                    Flow::ret(count)
                }
                END => {
                    m.cd_take(END, &done);
                    system_error(m, 0x17)
                }
                _ => {
                    m.cd_take(ERR, &done);
                    let error = system_error(m, 0x16);
                    crate::machine::seq(m, error, move |m, _| {
                        m.cd_block_read_attempt(count, sector, dst, left - 1)
                    })
                }
            })
        })
    }

    /// A(A6h) `CdGetStatus()` (OpenBIOS `cdromBlockGetStatus`): the drive's
    /// status byte kept in the driver state.
    pub fn cd_block_status(&mut self) -> Result<Flow<S>, String> {
        if self.cd_get_status_at(None) == 0 {
            return Ok(system_error(self, 0x1F));
        }
        let done = self.kernel.cd.done.clone();
        let wait = cd_wait(self, done.clone());
        Ok(crate::machine::seq(self, wait, move |m, result| {
            match result as u32 {
                DNE => {
                    m.cd_take(DNE, &done);
                    Flow::ret((result >> 32) as u32)
                }
                _ => {
                    m.cd_take(ERR, &done);
                    system_error(m, 0x20)
                }
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bios;
    use crate::testing::machine_with_disc;

    fn sector(lba: u32, data: &[u8]) -> [u8; cd::RAW] {
        let mut s = [0u8; cd::RAW];
        s[..12].copy_from_slice(&[
            0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0,
        ]);
        s[12..15].copy_from_slice(&cd::lba_to_msf(lba));
        s[15] = 2;
        s[0x18..0x18 + data.len()].copy_from_slice(data);
        let e = cd::edc(&s[0x10..0x818]);
        s[0x818..0x81C].copy_from_slice(&e.to_le_bytes());
        s
    }

    fn run(m: &mut Machine<crate::testing::NoSdk>, mut flow: Flow<crate::testing::NoSdk>) -> u64 {
        loop {
            match flow {
                Flow::Return(v) => return v,
                Flow::Call { .. } => panic!("no guest in this test"),
                Flow::Block { until, then } => {
                    while !m.holds(until) {
                        let limit = m.clock.next_time().unwrap_or(m.clock.now + 1000);
                        m.step(limit.max(m.clock.now));
                    }
                    flow = then(m);
                }
            }
        }
    }

    fn wait(m: &mut Machine<crate::testing::NoSdk>, at: u64) {
        // A zero-duration seek still has a completion queued at `now`.
        m.step(at);
        run(
            m,
            Flow::Block {
                until: Wait::Cycle(at),
                then: Box::new(|_| Flow::ret(0)),
            },
        );
    }

    fn ret(f: Result<Flow<crate::testing::NoSdk>, String>) -> u32 {
        match f.unwrap() {
            Flow::Return(v) => v as u32,
            _ => panic!("blocked"),
        }
    }

    /// The fallback core's command path, as an SDK caller uses it.
    fn core_cmd(m: &mut Machine<crate::testing::NoSdk>, cmd: u8) -> Vec<u8> {
        let now = m.clock.now;
        m.io.as_mut().unwrap().cdc.issue(cmd, &[], now);
        m.cdc_sync();
        let at = m.clock.now + m.cd_drive().ack_cycles();
        wait(m, at);
        let io = m.io.as_mut().unwrap();
        assert_eq!(io.cdc.pending(), 3, "INT3 for command {cmd:#04x}");
        let response = io.cdc.take_response(16);
        io.cdc.acknowledge();
        response
    }

    /// Seek, status, a two-sector read, then a missing sector: guest
    /// buffers, results, ACK-then-DNE order, no END on success, the
    /// fallback controller's own query state, and no preexisting core work
    /// continuing over the BIOS operation.
    #[test]
    fn native_cd_seek_read_status_and_failure() {
        let mut sectors = vec![[0u8; cd::RAW]; 8];
        sectors[2] = sector(2, &[0xAB; cd::USER]);
        let mut bad = sector(3, &[0xCD; cd::USER]);
        bad[0x20] ^= 1; // a sector whose EDC fails, still delivered as the core does
        sectors[3] = bad;
        let mut m = machine_with_disc(sectors);
        m.io = Some(Default::default());
        m.drive.timing = cd::Timing::Instant;
        // A core read with a raised IRQ line and a pending sector job.
        m.io.as_mut().unwrap().cdc.set_enable(0x1F);
        m.io.as_mut().unwrap().cdc.issue(0x06, &[], 0);
        m.cdc_sync();
        let at = m.clock.now + m.cd_drive().ack_cycles();
        wait(&mut m, at);
        assert!(m.io.as_ref().unwrap().cdc_line);
        assert!(m.clock.pending(|e| *e == Event::Cdc).is_some());

        assert_eq!(ret(bios::call(&mut m, 0xA0, 0x54, [0; 4])), 0, "_96_init");
        assert!(m.cd_installed());
        assert_eq!(m.cd_status(), 0x02);
        assert_eq!(
            m.io.as_ref().unwrap().cdc.next_due(),
            None,
            "preexisting core work stopped"
        );
        assert_eq!(
            m.clock.pending(|e| *e == Event::Cdc),
            None,
            "no stale controller wake"
        );
        assert!(
            !m.io.as_ref().unwrap().cdc_line,
            "line lowered for the next core IRQ"
        );
        assert_eq!(m.io.as_ref().unwrap().cdc.drive.timing, cd::Timing::Instant);

        let msf = cd::lba_to_msf(2);
        m.mem.write(0x8001_0000, &msf);
        assert_eq!(
            ret(bios::call(&mut m, 0xA0, 0x78, [0x8001_0000, 0, 0, 0])),
            1,
            "CdAsyncSeekL"
        );
        assert_eq!(m.kernel.cd.last, None);
        assert_eq!(
            m.cd_due(),
            m.clock.now,
            "native seek uses the configured timing"
        );
        assert_eq!(
            m.io.as_ref().unwrap().cdc.drive.head,
            2,
            "the fallback controller's drive moved"
        );
        let at = m.cd_due();
        wait(&mut m, at);
        assert_eq!(m.kernel.cd.last, Some(DNE));
        assert_eq!(m.cd_status(), 0x02);

        assert_eq!(
            ret(bios::call(&mut m, 0xA0, 0x7C, [0x8002_0000, 0, 0, 0])),
            1,
            "CdAsyncGetStatus"
        );
        let at = m.cd_due();
        wait(&mut m, at);
        assert_eq!(m.mem.r8(0x8002_0000), 0x02, "GetStat's byte");
        assert_eq!(m.kernel.cd.last, Some(DNE));

        assert_eq!(
            ret(bios::call(&mut m, 0xA0, 0x81, [0x20, 0, 0, 0])),
            1,
            "CdAsyncSetMode"
        );
        let at = m.cd_due();
        wait(&mut m, at);
        assert_eq!(m.cd_drive().mode, 0x20);
        assert_eq!(m.kernel.cd.last, Some(DNE));

        // A read's last buffer reports ACK; the normal pause then DNE.
        for (spec, func) in [(ACK, 0x8000_2000), (DNE, 0x8000_2001)] {
            let h = ret(bios::call(
                &mut m,
                0xB0,
                0x08,
                [EV_CD, spec, kernel::EV_MODE_INTR, func],
            ));
            ret(bios::call(&mut m, 0xB0, 0x0C, [h, 0, 0, 0]));
        }
        m.calls.clear();
        assert_eq!(
            ret(bios::call(&mut m, 0xA0, 0x7E, [2, 0x8003_0000, 0, 0])),
            1,
            "CdAsyncReadSector"
        );
        let at = m.cd_due();
        wait(&mut m, at);
        assert_eq!(
            m.mem.bytes(0x8003_0000, 4),
            [0xAB; 4],
            "the first sector before the last"
        );
        assert_eq!(m.kernel.cd.last, None);
        let at = m.cd_due();
        wait(&mut m, at);
        assert_eq!(
            m.mem.bytes(0x8003_0800, 4),
            [0xCD; 4],
            "the second sector after it"
        );
        assert_eq!(m.kernel.cd.last, Some(ACK), "the last buffer's completion");
        assert_eq!(
            m.calls.iter().map(|c| c.addr).collect::<Vec<_>>(),
            [0x8000_2000]
        );
        assert_ne!(
            m.events().iter().find(|e| e.spec == END).unwrap().status,
            kernel::EV_READY,
            "no early end"
        );
        let at = m.cd_due();
        wait(&mut m, at);
        assert_eq!(m.kernel.cd.last, Some(DNE), "the normal pause completion");
        assert_eq!(
            m.calls.iter().map(|c| c.addr).collect::<Vec<_>>(),
            [0x8000_2000, 0x8000_2001]
        );
        assert_eq!(m.cd_status(), 0x02);
        assert!(
            m.io.as_ref()
                .unwrap()
                .cdc
                .edc_errors
                .iter()
                .any(|e| e.contains("LBA 3")),
            "EDC checked"
        );

        // The core's own query state sees the BIOS read's last header and
        // status, and its sector count is only the BIOS's two.
        let loc = core_cmd(&mut m, 0x10);
        let expected = &sector(3, &[0xCD; cd::USER])[12..20];
        assert_eq!(loc, expected, "GetlocL sees the BIOS read's header");
        assert_eq!(
            core_cmd(&mut m, 0x01),
            [0x02],
            "GetStat after the BIOS read"
        );
        assert_eq!(
            m.io.as_ref().unwrap().cdc.sectors,
            2,
            "no preexisting core sector ran"
        );

        // A missing sector is an early end, not an invented INT5. The
        // failed sector's destination stays untouched and the next status
        // is the ordinary motor+standby byte.
        let msf = cd::lba_to_msf(40);
        m.mem.write(0x8001_0000, &msf);
        assert_eq!(
            ret(bios::call(&mut m, 0xA0, 0x78, [0x8001_0000, 0, 0, 0])),
            1
        );
        let at = m.cd_due();
        wait(&mut m, at);
        m.mem.write(0x8004_0000, b"keep");
        assert_eq!(
            ret(bios::call(&mut m, 0xA0, 0x7E, [1, 0x8004_0000, 0, 0])),
            1
        );
        let at = m.cd_due();
        wait(&mut m, at);
        assert_eq!(m.kernel.cd.last, Some(END));
        assert_eq!(
            m.mem.bytes(0x8004_0000, 4),
            b"keep",
            "the failed sector wrote nothing"
        );
        assert_eq!(
            m.events().iter().find(|e| e.spec == END).unwrap().status,
            kernel::EV_READY
        );
        assert_eq!(m.cd_status(), 0x02, "no sticky read/error bit");
        ret(bios::call(
            &mut m,
            0xA0,
            0x94,
            [0x8005_0000, 0x8005_0001, 0, 0],
        ));
        assert_eq!(m.mem.bytes(0x8005_0000, 2), [0, 0], "no fabricated INT5");
        let f = bios::call(&mut m, 0xA0, 0xA6, [0; 4]).unwrap();
        assert_eq!(run(&mut m, f), 0x02, "the next status is ordinary");
    }

    /// `CdReadSector` waits through every sector and the pause, filling the
    /// destination; its seek/read completions are in callback order and
    /// consumed.
    #[test]
    fn blocking_cd_calls_wait_for_their_events() {
        let mut sectors = vec![[0u8; cd::RAW]; 8];
        sectors[2] = sector(2, b"hello");
        sectors[3] = sector(3, b"world");
        let mut m = machine_with_disc(sectors);
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0x54, [0; 4])), 0, "_96_init");
        for (spec, func) in [(DNE, 0x8000_1000), (ACK, 0x8000_1001)] {
            let h = ret(bios::call(
                &mut m,
                0xB0,
                0x08,
                [EV_CD, spec, kernel::EV_MODE_INTR, func],
            ));
            ret(bios::call(&mut m, 0xB0, 0x0C, [h, 0, 0, 0]));
        }
        let start = m.clock.now;
        let sector_time = m.cd_drive().sector_cycles();
        let f = bios::call(&mut m, 0xA0, 0xA5, [2, 2, 0x8001_0000, 0]).unwrap();
        assert_eq!(run(&mut m, f), 2);
        assert_eq!(m.mem.bytes(0x8001_0000, 5), b"hello");
        assert_eq!(
            m.mem.bytes(0x8001_0800, 5),
            b"world",
            "the second sector, not just the first"
        );
        assert_eq!(
            m.drive.head, 4,
            "bus-free native reads advance the same drive"
        );
        assert!(
            m.clock.now >= start + 2 * sector_time,
            "both sectors took their time"
        );
        assert_eq!(
            m.calls.iter().map(|c| c.addr).collect::<Vec<_>>(),
            [0x8000_1000, 0x8000_1001, 0x8000_1000]
        );
        assert_eq!(
            m.events().iter().find(|e| e.spec == DNE).unwrap().status,
            kernel::EV_ENABLED,
            "DNE consumed"
        );
        let f = bios::call(&mut m, 0xA0, 0xA6, [0; 4]).unwrap();
        assert_eq!(run(&mut m, f), 2, "motor on, standby");
        // The inner init keeps event handles and cwd, and leaves critical.
        let events = m.kernel.cd.events;
        m.kernel.files.cd_cwd = "DATA".into();
        m.crit = true;
        assert_eq!(
            ret(bios::call(&mut m, 0xA0, 0x95, [0; 4])),
            1,
            "CdInitSubFunc"
        );
        assert_eq!(m.kernel.cd.events, events);
        assert_eq!(m.kernel.files.cd_cwd, "DATA");
        assert!(!m.crit);
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0x56, [0; 4])), 0, "_96_remove");
        assert!(!m.cd_installed());
        assert!(m.crit);
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0x95, [0; 4])), 1);
        assert!(!m.cd_installed(), "inner init does not allocate events");
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0x54, [0; 4])), 0);
        assert!(m.cd_installed());
        assert!(!m.crit);
        assert!(m.kernel.files.cd_cwd.is_empty());
    }

    /// Terminal failures call the patchable SystemError and return -1 if it returns.
    /// A completion callback may start another operation before a waiter resumes.
    #[test]
    fn blocking_results_survive_callbacks_and_errors_can_return() {
        let mut m = machine_with_disc(vec![]);
        m.drive.timing = cd::Timing::Instant;
        m.cd_init();
        let error = 0x8001_1000;
        m.mem.w32(crate::tables::entry(0xA0, 0xA1).unwrap(), error);
        let f = m.cd_block_read(1, 40, 0x8002_0000).unwrap();
        m.mem.write(0x8002_0000, b"keep");
        let mut f = f;
        loop {
            match f {
                Flow::Block { until, then } => {
                    while !m.holds(until) {
                        let at = m.clock.next_time().unwrap();
                        m.step(at);
                    }
                    f = then(&mut m);
                }
                Flow::Call { call, then } => {
                    assert_eq!((call.addr, call.args), (error, [0x44, 0x17, 0, 0]));
                    let f = then(&mut m, 123);
                    assert_eq!(run(&mut m, f), u32::MAX as u64);
                    break;
                }
                Flow::Return(_) => panic!("SystemError was not called"),
            }
        }
        assert_eq!(m.mem.bytes(0x8002_0000, 4), b"keep");

        let h = m.open_event(EV_CD, DNE, kernel::EV_MODE_INTR, 0x8001_2000);
        m.set_ev_status(h, kernel::EV_ENABLED);
        let f = m.cd_block_status().unwrap();
        let Flow::Block { until, then } = f else {
            panic!("status should wait")
        };
        while !m.holds(until) {
            let at = m.clock.next_time().unwrap();
            m.step(at);
        }
        let callback = m.calls.pop_front().expect("completion callback");
        assert_eq!(callback.addr, 0x8001_2000);
        // Execute the callback's BIOS call before the wait continuation,
        // as the runtime drains callbacks between scheduler steps.
        assert_eq!(
            ret(bios::call(&mut m, 0xA0, 0x7C, [0x8002_1000, 0, 0, 0])),
            1
        );
        m.drive.status = 0xEE;
        let at = m.cd_due();
        wait(&mut m, at);
        assert_eq!(m.kernel.cd.last_status, 0xEE);
        let f = then(&mut m);
        assert_eq!(
            run(&mut m, f),
            2,
            "the completed status, not the callback's operation"
        );
        assert!(
            m.event_ready(m.kernel.cd.events[1]),
            "the new operation's event is not consumed"
        );
        m.cd_remove();
        let Flow::Call { call, then } = m.cd_block_status().unwrap() else {
            panic!("SystemError")
        };
        assert_eq!((call.addr, call.args), (error, [0x44, 0x1F, 0, 0]));
        let f = then(&mut m, 0);
        assert_eq!(run(&mut m, f), u32::MAX as u64);

        // Unlike a read ERR, seek/status errors are terminal even when the
        // patched error handler returns. Both consume ERR before calling it.
        for (func, code) in [(0xA5, 0x0C), (0xA6, 0x20)] {
            m.cd_init();
            let flow = bios::call(&mut m, 0xA0, func, [1, 40, 0x8002_0000, 0]).unwrap();
            let Flow::Block { until: Wait::Cycle(at), then } = flow else {
                panic!("operation must wait");
            };
            assert_eq!(ret(bios::call(&mut m, 0xA0, 0x95, [0; 4])), 1);
            ret(bios::call(&mut m, 0xB0, 0x07, [EV_CD, ERR, 0, 0]));
            m.step(at);
            let Flow::Call { call, then } = then(&mut m) else {
                panic!("SystemError");
            };
            assert_eq!((call.addr, call.args), (error, [0x44, code, 0, 0]));
            assert!(!m.event_ready(m.kernel.cd.events[4]));
            let flow = then(&mut m, 123);
            assert_eq!(run(&mut m, flow), u32::MAX as u64);
            assert!(m.kernel.cd.op.is_none(), "seek/status errors do not retry");
        }
    }

    /// Returning from a read error retries, but not before SystemError returns.
    /// The ten-attempt limit, retail's lost-minute bug, original buffer/count,
    /// and live error-table lookup survive the continuation boundary.
    #[test]
    fn blocking_read_retries_after_returning_system_error() {
        for failures in [1, 10] {
            let mut sectors = vec![[0u8; cd::RAW]; 4504];
            sectors[4502] = sector(4502, b"first");
            sectors[2] = sector(2, b"retry");
            sectors[3] = sector(3, b"tail");
            let mut m = machine_with_disc(sectors);
            m.drive.timing = cd::Timing::Instant;
            m.cd_init();
            let error_entry = crate::tables::entry(0xA0, 0xA1).unwrap();
            let error = 0x8001_1000;
            let dst = 0x8002_0000;
            m.mem.w32(error_entry, error);
            m.mem.write(dst + 0x800, b"keep");
            let mut flow = bios::call(&mut m, 0xA0, 0xA5, [2, 4502, dst, 0]).unwrap();
            for attempt in 0..failures {
                let Flow::Block { until: Wait::Cycle(at), then } = flow else {
                    panic!("seek must wait on attempt {attempt}");
                };
                assert_eq!(m.cd_drive().head, if attempt == 0 { 4502 } else { 2 });
                m.step(at);
                let flow_read = then(&mut m);
                let Flow::Block { until: Wait::Cycle(at), then } = flow_read else {
                    panic!("read must wait on attempt {attempt}");
                };
                // Complete one frame, then reset the pending read through A95.
                // This uses native cancellation, not a fabricated controller INT5.
                m.step(at);
                assert_eq!(m.mem.bytes(dst, 5), if attempt == 0 { b"first" } else { b"retry" });
                assert_eq!(m.mem.bytes(dst + 0x800, 4), b"keep");
                assert_eq!(ret(bios::call(&mut m, 0xA0, 0x95, [0; 4])), 1);
                ret(bios::call(&mut m, 0xB0, 0x07, [EV_CD, ERR, 0, 0]));
                let flow_error = then(&mut m);
                let Flow::Call { call, then } = flow_error else {
                    panic!("read error must call SystemError");
                };
                assert_eq!((call.addr, call.args), (error + attempt * 4, [0x44, 0x16, 0, 0]));
                assert!(!m.event_ready(m.kernel.cd.events[4]), "ERR consumed before callback");
                assert!(m.kernel.cd.op.is_none(), "retry waits for the guest handler");
                // A returning patch may edit the table for the next error.
                m.mem.w32(error_entry, error + (attempt + 1) * 4);
                flow = then(&mut m, 123);
            }
            if failures == 10 {
                let Flow::Call { call, then } = flow else {
                    panic!("exhaustion must call SystemError once more");
                };
                assert_eq!((call.addr, call.args), (error + 40, [0x44, 0x0C, 0, 0]));
                assert!(m.kernel.cd.op.is_none(), "no eleventh attempt");
                let flow = then(&mut m, 456);
                assert_eq!(run(&mut m, flow), u32::MAX as u64);
            } else {
                assert_eq!(run(&mut m, flow), 2, "original sector count");
                assert_eq!(m.mem.bytes(dst, 5), b"retry", "retry starts at original buffer");
                assert_eq!(m.mem.bytes(dst + 0x800, 4), b"tail");
                assert_eq!(m.cd_drive().head, 4, "retail retry loses the first minute");
            }
        }
    }

    /// A partial `_96_init` leaves no event open, and `_96_remove` does not
    /// close a slot the guest reused for a different event.
    #[test]
    fn cd_event_ownership_is_exact() {
        let mut m = crate::testing::machine();
        m.lay_out_events(1);
        assert_eq!(
            ret(bios::call(&mut m, 0xA0, 0x54, [0; 4])),
            u32::MAX,
            "not enough event blocks"
        );
        assert!(!m.cd_installed());
        assert!(m.events().iter().all(|e| e.status == kernel::EV_UNUSED));

        let mut m = crate::testing::machine();
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0x54, [0; 4])), 0);
        let h = m.kernel.cd.events[0];
        ret(bios::call(&mut m, 0xB0, 0x09, [h, 0, 0, 0]));
        let guest = ret(bios::call(
            &mut m,
            0xB0,
            0x08,
            [0xF200_0002, 2, kernel::EV_MODE_NOINTR, 0],
        ));
        assert_eq!(guest, h, "the guest reuses the slot");
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0x56, [0; 4])), 0);
        assert_eq!(
            m.ev(guest).unwrap().status,
            kernel::EV_DISABLED,
            "the guest's event is untouched"
        );
    }
}
