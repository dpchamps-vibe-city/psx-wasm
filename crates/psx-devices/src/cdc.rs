//! The CD-ROM controller at register level (D58; psx-spx "CDROM Controller
//! I/O Ports" and "CDROM Controller Command Summary"), for SDK code that
//! drives the drive itself: libcd objects with no validated adapter, and
//! libds.
//!
//! Four ports at 1F801800h-1F801803h, banked by the index in the first.
//! A command byte, with its parameters queued before it, gets a first
//! response (INT3, or INT5 on error) after the controller's latency, and
//! some commands a second one (INT2) when the drive has done the work.
//! Only one interrupt is presented at a time: the rest wait until the CPU
//! acknowledges it. Reading delivers a sector per sector period (INT1),
//! into the sector buffer the CPU then moves through the data FIFO, by
//! byte or by DMA3. XA-ADPCM sectors go to the decoder instead
//! ([`crate::xa`]), and their samples are the SPU's CD input.
//!
//! Time is the machine's virtual time: the bus asks [`Cdc::next_due`] when
//! to call [`Cdc::run`] again, and the latencies come from the
//! [`crate::cd::Drive`] timing model (realistic or instant).

use crate::cd::{self, Disc, Drive, RAW, Timing};
use crate::xa;
use std::collections::VecDeque;

pub const BASE: u32 = 0x1F80_1800;

/// Interrupt types.
pub const INT1: u8 = 1;
pub const INT2: u8 = 2;
pub const INT3: u8 = 3;
pub const INT5: u8 = 5;

/// Status byte bits.
pub mod stat {
    pub const ERROR: u8 = 0x01;
    pub const MOTOR: u8 = 0x02;
    pub const READ: u8 = 0x20;
    pub const SEEK: u8 = 0x40;
}

/// Samples of decoded CD audio kept for the SPU: a second.
const AUDIO_MAX: usize = 44_100;

#[derive(Debug, Clone)]
struct Pending {
    int: u8,
    response: Vec<u8>,
    /// INT1's sector, which becomes the sector buffer when presented.
    sector: Option<Box<[u8; RAW]>>,
}

/// Work the drive has scheduled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Job {
    /// A command's first response.
    Ack(u8),
    /// A command's second response (INT2).
    Done(u8),
    /// The next sector of a read comes under the head.
    Sector,
}

#[derive(Debug, Clone)]
pub struct Cdc {
    pub drive: Drive,
    index: u8,
    params: Vec<u8>,
    /// The parameters of the command being processed.
    cmd_params: Vec<u8>,
    response: VecDeque<u8>,
    /// The interrupt presented (0: none).
    int: u8,
    int_enable: u8,
    queue: VecDeque<Pending>,
    jobs: Vec<(u64, Job)>,
    /// A command was written and its first response isn't out yet.
    busy: bool,
    sector: Option<Box<[u8; RAW]>>,
    fifo: Vec<u8>,
    fifo_pos: usize,
    motor: bool,
    reading: bool,
    seeking: bool,
    setloc_pending: bool,
    filter: (u8, u8),
    muted: bool,
    xa: xa::Decoder,
    resample: [xa::Resampler; 2],
    /// The CD-to-SPU volume matrix (L→L, L→R, R→R, R→L): pending, applied.
    vol_next: [u8; 4],
    vol: [u8; 4],
    /// The last data sector's header and subheader (GetlocL).
    loc: [u8; 8],
    /// Track start LBAs, and the disc's end.
    pub tracks: Vec<u32>,
    pub end: u32,
    /// GetID's region string.
    pub region: [u8; 4],
    /// Decoded CD audio, for the SPU's CD input.
    pub audio: VecDeque<[i16; 2]>,
    pub sectors: u64,
    pub xa_sectors: u64,
    pub edc_errors: Vec<String>,
}

impl Default for Cdc {
    fn default() -> Self {
        Cdc {
            drive: Drive::default(),
            index: 0,
            params: Vec::new(),
            cmd_params: Vec::new(),
            response: VecDeque::new(),
            int: 0,
            // As the BIOS leaves it after reading the boot disc: libcd's
            // CD_init only rewrites it when flags are pending.
            int_enable: 0x1F,
            queue: VecDeque::new(),
            jobs: Vec::new(),
            busy: false,
            sector: None,
            fifo: Vec::new(),
            fifo_pos: 0,
            motor: true,
            reading: false,
            seeking: false,
            setloc_pending: false,
            filter: (0, 0),
            muted: false,
            xa: xa::Decoder::default(),
            resample: Default::default(),
            vol_next: [0x80, 0, 0x80, 0],
            vol: [0x80, 0, 0x80, 0],
            loc: [0; 8],
            tracks: vec![0],
            end: 0,
            region: *b"SCEI",
            audio: VecDeque::new(),
            sectors: 0,
            xa_sectors: 0,
            edc_errors: Vec::new(),
        }
    }
}

fn bcd(v: u32) -> u8 {
    (((v / 10) << 4) | (v % 10)) as u8
}

fn from_bcd(b: u8) -> u32 {
    (b >> 4) as u32 * 10 + (b & 15) as u32
}

impl Cdc {
    pub fn handles(addr: u32) -> bool {
        (BASE..BASE + 4).contains(&addr)
    }

    fn stat(&self) -> u8 {
        let mut s = 0;
        if self.motor {
            s |= stat::MOTOR;
        }
        if self.seeking {
            s |= stat::SEEK;
        } else if self.reading {
            s |= stat::READ;
        }
        s
    }

    /// Whether the controller's interrupt line (IRQ2) is up.
    pub fn line(&self) -> bool {
        self.int & self.int_enable & 0x1F != 0
    }

    /// The earliest cycle with work due.
    pub fn next_due(&self) -> Option<u64> {
        self.jobs.iter().map(|j| j.0).min()
    }

    fn schedule(&mut self, at: u64, job: Job) {
        self.jobs.push((at, job));
    }

    fn cancel(&mut self, f: impl Fn(Job) -> bool) {
        self.jobs.retain(|&(_, j)| !f(j));
    }

    /// Present an interrupt, or queue it behind the one presented.
    fn interrupt(&mut self, int: u8, response: Vec<u8>, sector: Option<Box<[u8; RAW]>>) {
        let p = Pending { int, response, sector };
        if self.int == 0 && self.queue.is_empty() {
            self.present(p);
        } else {
            self.queue.push_back(p);
        }
    }

    fn present(&mut self, p: Pending) {
        self.int = p.int;
        self.response = p.response.into();
        if let Some(s) = p.sector {
            self.sector = Some(s);
        }
    }

    pub fn read(&mut self, addr: u32) -> u8 {
        match (addr - BASE, self.index) {
            (0, _) => {
                let i = self.index;
                i | (self.params.is_empty() as u8) << 3
                    | ((self.params.len() < 16) as u8) << 4
                    | (!self.response.is_empty() as u8) << 5
                    | ((self.fifo_pos < self.fifo.len()) as u8) << 6
                    | (self.busy as u8) << 7
            }
            (1, _) => self.response.pop_front().unwrap_or(0),
            (2, _) => self.fifo_byte(),
            (3, 0 | 2) => 0xE0 | self.int_enable,
            (3, _) => 0xE0 | self.int,
            _ => 0,
        }
    }

    fn fifo_byte(&mut self) -> u8 {
        let b = self.fifo.get(self.fifo_pos).copied().unwrap_or(0);
        if self.fifo_pos < self.fifo.len() {
            self.fifo_pos += 1;
        }
        b
    }

    /// One word of the data FIFO (DMA3).
    pub fn fifo_word(&mut self) -> u32 {
        u32::from_le_bytes([self.fifo_byte(), self.fifo_byte(), self.fifo_byte(), self.fifo_byte()])
    }

    pub fn write(&mut self, addr: u32, v: u8, now: u64) {
        match (addr - BASE, self.index) {
            (0, _) => self.index = v & 3,
            (1, 0) => self.command(v, now),
            (2, 0) => {
                if self.params.len() < 16 {
                    self.params.push(v);
                }
            }
            (3, 0) => {
                // Request: BFRD loads the sector buffer into the data FIFO.
                if v & 0x80 != 0 {
                    if self.fifo_pos >= self.fifo.len() {
                        self.fill_fifo();
                    }
                } else {
                    self.fifo.clear();
                    self.fifo_pos = 0;
                }
            }
            (2, 1) => self.int_enable = v & 0x1F,
            (3, 1) => {
                // Acknowledge: the flags written as 1 clear.
                if v & 0x40 != 0 {
                    self.params.clear();
                }
                if v & 0x07 == 0x07 || self.int & v & 0x07 == self.int {
                    self.int = 0;
                    self.response.clear();
                    if let Some(p) = self.queue.pop_front() {
                        self.present(p);
                    }
                }
            }
            (2, 2) => self.vol_next[0] = v,
            (3, 2) => self.vol_next[1] = v,
            (1, 3) => self.vol_next[2] = v,
            (2, 3) => self.vol_next[3] = v,
            (3, 3) => {
                if v & 0x20 != 0 {
                    self.vol = self.vol_next;
                }
            }
            _ => {}
        }
    }

    fn fill_fifo(&mut self) {
        let Some(s) = &self.sector else {
            self.fifo.clear();
            self.fifo_pos = 0;
            return;
        };
        self.fifo = if self.drive.mode & cd::mode::SIZE_2340 != 0 { s[12..12 + 0x924].to_vec() } else { s[24..24 + 0x800].to_vec() };
        self.fifo_pos = 0;
    }

    fn command(&mut self, c: u8, now: u64) {
        self.cmd_params = std::mem::take(&mut self.params);
        self.busy = true;
        self.cancel(|j| matches!(j, Job::Ack(_)));
        self.schedule(now + self.drive.ack_cycles(), Job::Ack(c));
    }

    /// Do everything due by `now`, oldest first.
    pub fn run(&mut self, now: u64, disc: &mut dyn Disc) {
        loop {
            let Some(i) = self.jobs.iter().enumerate().filter(|(_, j)| j.0 <= now).min_by_key(|(_, j)| j.0).map(|(i, _)| i) else {
                return;
            };
            let (at, job) = self.jobs.remove(i);
            match job {
                Job::Ack(c) => self.ack(c, at),
                Job::Done(c) => self.done(c),
                Job::Sector => self.next_sector(at, disc),
            }
        }
    }

    fn sector_cycles(&self) -> u64 {
        // XA audio plays at the disc's real rate even under instant timing.
        if self.drive.mode & cd::mode::XA_ADPCM != 0 && self.drive.timing == Timing::Instant {
            let speed = if self.drive.mode & cd::mode::DOUBLE_SPEED != 0 { 150 } else { 75 };
            crate::clock::CPU_HZ / speed
        } else {
            self.drive.sector_cycles()
        }
    }

    fn stop_reading(&mut self) {
        self.reading = false;
        self.seeking = false;
        self.cancel(|j| j == Job::Sector);
    }

    /// Move the head to the Setloc target, if one is pending: the seek's
    /// duration.
    fn seek(&mut self) -> u64 {
        if !self.setloc_pending {
            return 0;
        }
        self.setloc_pending = false;
        let t = self.drive.seek_cycles(self.drive.head, self.drive.setloc);
        self.drive.head = self.drive.setloc;
        t
    }

    /// The first response, and whatever the command starts.
    fn ack(&mut self, c: u8, now: u64) {
        self.busy = false;
        self.drive.last_command = c;
        let p = std::mem::take(&mut self.cmd_params);
        let arg = |i: usize| p.get(i).copied().unwrap_or(0);
        let stat = self.stat();
        let mut res = vec![stat];
        match c {
            0x01 => {}
            0x02 => {
                self.drive.setloc = cd::msf_to_lba([arg(0), arg(1), arg(2)]);
                self.setloc_pending = true;
            }
            0x03 => {
                // Play: no audio tracks are played (CD-DA); the drive just
                // reports playing nothing.
                self.stop_reading();
            }
            0x06 | 0x1B => {
                self.stop_reading();
                self.motor = true;
                let seek = self.seek();
                self.seeking = seek > 0;
                self.reading = true;
                let t = now + seek + self.sector_cycles();
                self.schedule(t, Job::Sector);
            }
            0x07 => {
                self.motor = true;
                self.schedule(now + self.drive.ack_cycles() * 4, Job::Done(c));
            }
            0x08 => {
                self.stop_reading();
                self.schedule(now + self.drive.ack_cycles() * 4, Job::Done(c));
            }
            0x09 => {
                let was = self.reading;
                self.stop_reading();
                let t = if was { self.drive.sector_cycles() } else { self.drive.ack_cycles() };
                self.schedule(now + t, Job::Done(c));
            }
            0x0A => {
                self.stop_reading();
                self.drive.mode = 0x20;
                self.motor = true;
                self.schedule(now + self.drive.ack_cycles() * 2, Job::Done(c));
            }
            0x0B => self.muted = true,
            0x0C => self.muted = false,
            0x0D => self.filter = (arg(0), arg(1)),
            0x0E => self.drive.mode = arg(0),
            0x0F => res.extend([self.drive.mode, 0, self.filter.0, self.filter.1]),
            0x10 => res = self.loc.to_vec(),
            0x11 => {
                let lba = self.drive.head;
                let track = self.tracks.iter().rposition(|&t| t <= lba).unwrap_or(0);
                let rel = cd::lba_to_msf(lba - self.tracks[track]);
                // Relative to the track start: lba_to_msf adds the pregap.
                let rel = [rel[0], bcd(from_bcd(rel[1]).saturating_sub(2)), rel[2]];
                let abs = cd::lba_to_msf(lba);
                res = vec![bcd(track as u32 + 1), 1, rel[0], rel[1], rel[2], abs[0], abs[1], abs[2]];
            }
            0x12 => self.schedule(now + self.drive.ack_cycles() * 4, Job::Done(c)),
            0x13 => res.extend([1, bcd(self.tracks.len() as u32)]),
            0x14 => {
                let n = from_bcd(arg(0)) as usize;
                let lba = if n == 0 { self.end } else { self.tracks.get(n - 1).copied().unwrap_or(self.end) };
                let msf = cd::lba_to_msf(lba);
                res.extend([msf[0], msf[1]]);
            }
            0x15 | 0x16 => {
                self.stop_reading();
                let t = self.seek();
                self.seeking = true;
                self.schedule(now + t.max(1), Job::Done(c));
            }
            0x19 => {
                res = match arg(0) {
                    0x20 => vec![0x94, 0x09, 0x19, 0xC0],
                    _ => vec![stat],
                };
            }
            0x1A => self.schedule(now + self.drive.ack_cycles() / 2, Job::Done(c)),
            0x1C => {
                self.stop_reading();
                self.drive.mode = 0;
            }
            0x1E => self.schedule(now + self.drive.ack_cycles() * 8, Job::Done(c)),
            _ => {
                self.interrupt(INT5, vec![stat | stat::ERROR, 0x40], None);
                return;
            }
        }
        self.interrupt(INT3, res, None);
    }

    /// A command's second response.
    fn done(&mut self, c: u8) {
        match c {
            0x08 => self.motor = false,
            0x15 | 0x16 => self.seeking = false,
            _ => {}
        }
        let res = match c {
            0x1A => {
                let mut r = vec![stat::MOTOR, 0x00, 0x20, 0x00];
                r.extend(self.region);
                r
            }
            _ => vec![self.stat()],
        };
        self.interrupt(INT2, res, None);
    }

    /// The head reaches the next sector of a read.
    fn next_sector(&mut self, now: u64, disc: &mut dyn Disc) {
        self.seeking = false;
        let lba = self.drive.head;
        self.drive.head += 1;
        let t = now + self.sector_cycles();
        self.schedule(t, Job::Sector);
        let mut raw = Box::new([0u8; RAW]);
        if !disc.read_raw(lba, &mut raw) {
            self.edc_errors.push(format!("LBA {lba}: unreadable"));
            return;
        }
        if let Err(e) = cd::check(&raw, lba) {
            self.edc_errors.push(e);
        }
        self.sectors += 1;
        self.loc.copy_from_slice(&raw[12..20]);
        if self.xa_sector(&raw) {
            return;
        }
        // An undelivered sector is lost to the newer one, as when the CPU
        // falls behind on hardware.
        self.queue.retain(|p| p.int != INT1);
        if self.int == INT1 {
            self.sector = Some(raw);
            self.response = vec![self.stat()].into();
            return;
        }
        self.interrupt(INT1, vec![self.stat()], Some(raw));
    }

    /// With XA-ADPCM on, a Form2 audio sector goes to the decoder (when it
    /// passes the filter) instead of the CPU.
    fn xa_sector(&mut self, raw: &[u8; RAW]) -> bool {
        let sh = xa::Subheader::parse(raw);
        if self.drive.mode & cd::mode::XA_ADPCM == 0 || !sh.is_audio() {
            return false;
        }
        if self.drive.mode & cd::mode::FILTER != 0 && (sh.file, sh.channel) != self.filter {
            return true;
        }
        let s = self.xa.decode(raw);
        let (l, r) = xa::resample(&s, &mut self.resample);
        self.xa_sectors += 1;
        if !self.muted {
            let [ll, lr, rr, rl] = self.vol.map(|v| v as i32);
            self.audio.extend(l.into_iter().zip(r).map(|(l, r)| {
                let (l, r) = (l as i32, r as i32);
                [((l * ll + r * rl) >> 7).clamp(-0x8000, 0x7FFF) as i16, ((l * lr + r * rr) >> 7).clamp(-0x8000, 0x7FFF) as i16]
            }));
            let over = self.audio.len().saturating_sub(AUDIO_MAX);
            self.audio.drain(..over);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sectors whose user data is filled with their LBA's low byte.
    struct Fill;
    impl Disc for Fill {
        fn read_raw(&mut self, lba: u32, out: &mut [u8; RAW]) -> bool {
            *out = [0; RAW];
            out[..12].copy_from_slice(&[0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0]);
            out[12..15].copy_from_slice(&cd::lba_to_msf(lba));
            out[15] = 2;
            out[0x18..0x818].fill(lba as u8);
            let e = cd::edc(&out[0x10..0x818]);
            out[0x818..0x81C].copy_from_slice(&e.to_le_bytes());
            true
        }
    }

    fn cmd(c: &mut Cdc, now: &mut u64, op: u8, params: &[u8]) {
        c.write(BASE, 0, *now);
        for &p in params {
            c.write(BASE + 2, p, *now);
        }
        c.write(BASE + 1, op, *now);
        assert!(c.read(BASE) & 0x80 != 0, "busy until the first response");
    }

    fn until_int(c: &mut Cdc, now: &mut u64) -> u8 {
        while c.int == 0 {
            *now = c.next_due().expect("work due");
            c.run(*now, &mut Fill);
        }
        c.int
    }

    fn ack(c: &mut Cdc) -> Vec<u8> {
        let mut r = Vec::new();
        while c.read(BASE) & 0x20 != 0 {
            r.push(c.read(BASE + 1));
        }
        c.write(BASE, 1, 0);
        c.write(BASE + 3, 0x1F, 0);
        c.write(BASE, 0, 0);
        r
    }

    #[test]
    fn commands_answer_one_interrupt_at_a_time() {
        let mut c = Cdc { tracks: vec![0, 300_000], end: 320_000, ..Default::default() };
        let mut now = 0;
        c.write(BASE, 1, 0);
        c.write(BASE + 2, 0x1F, 0);
        c.write(BASE, 0, 0);
        cmd(&mut c, &mut now, 0x01, &[]);
        assert_eq!(until_int(&mut c, &mut now), INT3);
        assert!(c.line());
        assert_eq!(c.read(BASE + 3), 0xFF, "index 0 reads the enable register");
        c.write(BASE, 1, 0);
        assert_eq!(c.read(BASE + 3), 0xE0 | INT3, "index 1 the flags");
        c.write(BASE, 0, 0);
        assert_eq!(ack(&mut c), [stat::MOTOR]);
        assert!(!c.line());
        // GetTN, GetTD(2).
        cmd(&mut c, &mut now, 0x13, &[]);
        until_int(&mut c, &mut now);
        assert_eq!(ack(&mut c), [stat::MOTOR, 1, 2]);
        cmd(&mut c, &mut now, 0x14, &[2]);
        until_int(&mut c, &mut now);
        let msf = cd::lba_to_msf(300_000);
        assert_eq!(ack(&mut c), [stat::MOTOR, msf[0], msf[1]]);
        // GetID: INT3, then INT2 waits behind it until acknowledged.
        cmd(&mut c, &mut now, 0x1A, &[]);
        until_int(&mut c, &mut now);
        now = c.next_due().unwrap();
        c.run(now, &mut Fill);
        assert_eq!(c.int, INT3);
        ack(&mut c);
        assert_eq!(c.int, INT2);
        assert_eq!(ack(&mut c), [stat::MOTOR, 0, 0x20, 0, b'S', b'C', b'E', b'I']);
    }

    #[test]
    fn reads_deliver_sectors_through_the_fifo() {
        let mut c = Cdc::default();
        let mut now = 0;
        c.write(BASE, 1, 0);
        c.write(BASE + 2, 0x1F, 0);
        cmd(&mut c, &mut now, 0x0E, &[0x80]);
        until_int(&mut c, &mut now);
        ack(&mut c);
        let msf = cd::lba_to_msf(100);
        cmd(&mut c, &mut now, 0x02, &msf);
        until_int(&mut c, &mut now);
        ack(&mut c);
        cmd(&mut c, &mut now, 0x06, &[]);
        until_int(&mut c, &mut now);
        assert_eq!(ack(&mut c), [stat::MOTOR]);
        for lba in 100..103u32 {
            assert_eq!(until_int(&mut c, &mut now), INT1);
            assert_eq!(c.response.front(), Some(&(stat::MOTOR | stat::READ)));
            ack(&mut c);
            c.write(BASE + 3, 0x80, now);
            assert!(c.read(BASE) & 0x40 != 0);
            assert_eq!(c.fifo_word(), u32::from_le_bytes([lba as u8; 4]));
            for _ in 1..512 {
                c.fifo_word();
            }
            assert!(c.read(BASE) & 0x40 == 0, "2048 bytes");
            c.write(BASE + 3, 0, now);
        }
        // Pause: INT3, then INT2 once the drive has stopped.
        cmd(&mut c, &mut now, 0x09, &[]);
        assert_eq!(until_int(&mut c, &mut now), INT3);
        ack(&mut c);
        assert_eq!(until_int(&mut c, &mut now), INT2);
        assert_eq!(ack(&mut c), [stat::MOTOR]);
        assert_eq!(c.next_due(), None);
        assert!(c.edc_errors.is_empty());
    }
}
