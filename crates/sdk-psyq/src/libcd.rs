//! libcd's data path over the CD device: commands (`CdControl*`, `CdSync`),
//! `CdRead`/`CdReadSync`, the sector-stream interface (`CdReady`,
//! `CdGetSector`), callbacks, and `CdSearchFile`. Every sector comes from
//! the user's disc image and must pass its EDC; data lands in guest memory
//! as each sector arrives on the virtual timeline (realistic or instant
//! timing, D19). XA streaming and CD audio arrive with M6.
//!
//! Return conventions follow psyq-decomp `libcd`: `CdControl` returns 1
//! once acknowledged; `CdRead` returns 1 when started; `CdReadSync` returns
//! the sectors remaining (0 when done, -1 on error).

use crate::{Adapter, M, Result, block, cond, ev, ret};
use psx_bios::bios::HostDisc;
use psx_bios::{Event, Flow, GuestCall, Wait};
use psx_devices::cd::{self, mode, stat};

pub const ADAPTERS: &[(&str, Adapter)] = &[
    ("CdInit", cd_init),
    ("CdReset", cd_init),
    ("CdControl", cd_control),
    ("CdControlF", cd_control),
    ("CdControlB", cd_control_b),
    ("CdSync", cd_sync),
    ("CdReady", cd_ready),
    ("CdGetSector", cd_get_sector),
    ("CdGetSector2", cd_get_sector),
    ("CdRead", cd_read),
    ("CdReadSync", cd_read_sync),
    ("CdReadBreak", cd_read_break),
    ("CdReadCallback", cd_read_callback),
    ("CdSyncCallback", cd_sync_callback),
    ("CdReadyCallback", cd_ready_callback),
    ("CdDataCallback", cd_data_callback),
    ("CdFlush", zero),
    ("CdDataSync", zero),
    ("CdStatus", cd_status),
    ("CdMode", cd_mode),
    ("CdLastCom", cd_last_com),
    ("CdSearchFile", cd_search_file),
];

/// libcd command numbers.
pub mod com {
    pub const NOP: u8 = 1;
    pub const SETLOC: u8 = 2;
    pub const PLAY: u8 = 3;
    pub const READN: u8 = 6;
    pub const STANDBY: u8 = 7;
    pub const STOP: u8 = 8;
    pub const PAUSE: u8 = 9;
    pub const INIT: u8 = 10;
    pub const MUTE: u8 = 11;
    pub const DEMUTE: u8 = 12;
    pub const SETFILTER: u8 = 13;
    pub const SETMODE: u8 = 14;
    pub const GETPARAM: u8 = 15;
    pub const GETLOCL: u8 = 16;
    pub const GETLOCP: u8 = 17;
    pub const GETTN: u8 = 19;
    pub const GETTD: u8 = 20;
    pub const SEEKL: u8 = 21;
    pub const SEEKP: u8 = 22;
    pub const READS: u8 = 27;
}

/// Interrupt results passed to callbacks and returned by `CdSync`/`CdReady`.
pub const NO_INTR: u32 = 0;
pub const DATA_READY: u32 = 1;
pub const COMPLETE: u32 = 2;

#[derive(Debug, Clone, Copy)]
pub struct Read {
    pub next_lba: u32,
    pub remaining: u32,
    pub dst: u32,
    /// Bytes copied per sector: 2048, 2340 or 2328.
    pub size: u32,
    /// Offset of the copied part in the raw sector.
    pub skip: usize,
}

#[derive(Debug, Default)]
pub struct Cd {
    /// The pending command completes at this cycle.
    pub busy_until: Option<u64>,
    pub result: [u8; 8],
    pub sync_cb: u32,
    pub ready_cb: u32,
    pub read_cb: u32,
    pub data_cb: u32,
    pub read: Option<Read>,
    /// ReadN/ReadS issued through CdControl: the next LBA streamed.
    pub streaming: Option<u32>,
    pub ready_sector: Option<Box<[u8; cd::RAW]>>,
    /// Bumped to invalidate in-flight sector events.
    pub generation: u32,
    pub sectors: u64,
    pub edc_errors: Vec<String>,
}

fn status(m: &M) -> u8 {
    m.drive.status | stat::STANDBY
}

fn write_result(m: &mut M, at: u32, bytes: &[u8]) {
    if at != 0 {
        m.mem.write(at, bytes);
    }
}

/// Read one raw sector, verifying it; errors are recorded, not fatal here
/// (the boot check fails on any).
fn sector(m: &mut M, lba: u32) -> Option<Box<[u8; cd::RAW]>> {
    let mut raw = Box::new([0u8; cd::RAW]);
    if !m.host.disc_read(lba, &mut raw) {
        m.sdk.cd.edc_errors.push(format!("LBA {lba}: unreadable"));
        return None;
    }
    if let Err(e) = cd::check(&raw, lba) {
        m.sdk.cd.edc_errors.push(e);
    }
    m.sdk.cd.sectors += 1;
    Some(raw)
}

fn schedule_sector(m: &mut M, at: u64) {
    let g = m.sdk.cd.generation;
    m.clock.schedule(at, Event::Sdk(ev::CD_SECTOR, g));
}

fn stop_streams(m: &mut M) {
    m.sdk.cd.generation = m.sdk.cd.generation.wrapping_add(1);
    m.sdk.cd.read = None;
    m.sdk.cd.streaming = None;
    m.drive.status &= !(stat::READ | stat::SEEK);
}

/// Start a command: busy until it completes.
fn issue(m: &mut M, c: u8, extra: u64) {
    m.drive.last_command = c;
    let t = m.clock.now + m.drive.ack_cycles() + extra;
    m.sdk.cd.busy_until = Some(t);
    m.clock.cancel(|e| matches!(e, Event::Sdk(ev::CD_COMPLETE, _)));
    m.clock.schedule(t, Event::Sdk(ev::CD_COMPLETE, c as u32));
}

pub fn on_complete(m: &mut M, _c: u32) {
    m.sdk.cd.busy_until = None;
    let f = m.sdk.cd.sync_cb;
    if f != 0 {
        m.calls.push_back(GuestCall { addr: f, args: [COMPLETE, 0, 0, 0] });
    }
}

pub fn on_sector(m: &mut M, generation: u32) {
    if generation != m.sdk.cd.generation {
        return;
    }
    if let Some(r) = m.sdk.cd.read {
        let raw = sector(m, r.next_lba);
        if let Some(raw) = raw {
            let data = &raw[r.skip..r.skip + r.size as usize];
            m.mem.write(r.dst, data);
        }
        m.drive.head = r.next_lba + 1;
        let left = r.remaining - 1;
        if left == 0 {
            m.sdk.cd.read = None;
            m.drive.status &= !stat::READ;
            let f = m.sdk.cd.read_cb;
            if f != 0 {
                m.calls.push_back(GuestCall { addr: f, args: [COMPLETE, 0, 0, 0] });
            }
        } else {
            m.sdk.cd.read = Some(Read { next_lba: r.next_lba + 1, remaining: left, dst: r.dst + r.size, ..r });
            let t = m.clock.now + m.drive.sector_cycles();
            schedule_sector(m, t);
        }
        return;
    }
    if let Some(lba) = m.sdk.cd.streaming {
        m.sdk.cd.ready_sector = sector(m, lba);
        m.drive.head = lba + 1;
        m.sdk.cd.streaming = Some(lba + 1);
        let f = m.sdk.cd.ready_cb;
        if f != 0 {
            m.calls.push_back(GuestCall { addr: f, args: [DATA_READY, 0, 0, 0] });
        }
        let t = m.clock.now + m.drive.sector_cycles();
        schedule_sector(m, t);
    }
}

fn cd_init(m: &mut M, _a: [u32; 4]) -> Result {
    stop_streams(m);
    m.drive.mode = 0;
    m.drive.status = stat::STANDBY;
    m.sdk.cd.busy_until = None;
    ret(1)
}

fn control(m: &mut M, a: [u32; 4]) -> u32 {
    let c = a[0] as u8;
    let param = a[1];
    let p = |m: &M, i: u32| if param != 0 { m.mem.r8(param + i) } else { 0 };
    let mut extra = 0;
    let mut res: Vec<u8> = vec![status(m)];
    match c {
        com::SETLOC => {
            m.drive.setloc = cd::msf_to_lba([p(m, 0), p(m, 1), p(m, 2)]);
        }
        com::SETMODE => m.drive.mode = p(m, 0),
        com::READN | com::READS => {
            stop_streams(m);
            m.drive.status |= stat::READ;
            let seek = m.drive.seek_cycles(m.drive.head, m.drive.setloc);
            m.sdk.cd.streaming = Some(m.drive.setloc);
            let t = m.clock.now + seek + m.drive.sector_cycles();
            schedule_sector(m, t);
        }
        com::PAUSE | com::STOP | com::STANDBY | com::INIT => {
            stop_streams(m);
            if c == com::INIT {
                m.drive.mode = 0;
            }
            extra = m.drive.sector_cycles();
        }
        com::SEEKL | com::SEEKP => {
            stop_streams(m);
            extra = m.drive.seek_cycles(m.drive.head, m.drive.setloc);
            m.drive.head = m.drive.setloc;
        }
        com::GETPARAM => res.extend([m.drive.mode, 0, 0, 0]),
        com::GETLOCL | com::GETLOCP => {
            let msf = cd::lba_to_msf(m.drive.head);
            res = vec![1, 1, msf[0], msf[1], msf[2], msf[0], msf[1], msf[2]];
        }
        com::GETTN => res.extend([1, 1]),
        com::GETTD => res.extend([0, 2]),
        com::NOP | com::MUTE | com::DEMUTE | com::SETFILTER | com::PLAY => {}
        _ => m.log(&format!("CdControl: command {c} not modelled")),
    }
    issue(m, c, extra);
    m.sdk.cd.result[..res.len().min(8)].copy_from_slice(&res[..res.len().min(8)]);
    write_result(m, a[2], &res);
    1
}

fn cd_control(m: &mut M, a: [u32; 4]) -> Result {
    ret(control(m, a))
}

fn cd_control_b(m: &mut M, a: [u32; 4]) -> Result {
    let r = control(m, a);
    block(Wait::Sdk(cond::CD_COMMAND_DONE), move |_m: &mut M| Flow::ret(r))
}

fn cd_sync(m: &mut M, a: [u32; 4]) -> Result {
    let result = a[1];
    if a[0] == 0 {
        return block(Wait::Sdk(cond::CD_COMMAND_DONE), move |m: &mut M| {
            let s = status(m);
            write_result(m, result, &[s]);
            Flow::ret(COMPLETE)
        });
    }
    let s = status(m);
    write_result(m, result, &[s]);
    ret(if m.sdk.cd.busy_until.is_some() { NO_INTR } else { COMPLETE })
}

fn cd_ready(m: &mut M, a: [u32; 4]) -> Result {
    let result = a[1];
    if a[0] == 0 {
        return block(Wait::Sdk(cond::CD_DATA_READY), move |m: &mut M| {
            let s = status(m);
            write_result(m, result, &[s]);
            Flow::ret(if m.sdk.cd.ready_sector.is_some() { DATA_READY } else { NO_INTR })
        });
    }
    ret(if m.sdk.cd.ready_sector.is_some() { DATA_READY } else { NO_INTR })
}

fn cd_get_sector(m: &mut M, a: [u32; 4]) -> Result {
    let Some(raw) = m.sdk.cd.ready_sector.take() else { return ret(0) };
    let bytes = (a[1] as usize * 4).min(cd::RAW);
    let skip = if m.drive.mode & mode::SIZE_2340 != 0 { 12 } else { 0x18 };
    let end = (skip + bytes).min(cd::RAW);
    m.mem.write(a[0], &raw[skip..end]);
    ret(1)
}

fn cd_read(m: &mut M, a: [u32; 4]) -> Result {
    let (sectors, buf, md) = (a[0], a[1], a[2] as u8);
    stop_streams(m);
    m.drive.mode = md;
    let (size, skip) = match md & 0x30 {
        0x00 => (2048, 0x18),
        0x20 => (2340, 12),
        _ => (2328, 0x18),
    };
    if sectors == 0 {
        return ret(1);
    }
    m.sdk.cd.read = Some(Read { next_lba: m.drive.setloc, remaining: sectors, dst: buf, size, skip });
    m.drive.status |= stat::READ;
    m.drive.last_command = com::READN;
    let seek = m.drive.seek_cycles(m.drive.head, m.drive.setloc);
    let t = m.clock.now + m.drive.ack_cycles() + seek + m.drive.sector_cycles();
    schedule_sector(m, t);
    ret(1)
}

fn cd_read_sync(m: &mut M, a: [u32; 4]) -> Result {
    let result = a[1];
    if a[0] == 0 {
        return block(Wait::Sdk(cond::CD_READ_DONE), move |m: &mut M| {
            let s = status(m);
            write_result(m, result, &[s]);
            Flow::ret(0)
        });
    }
    ret(m.sdk.cd.read.map_or(0, |r| r.remaining))
}

fn cd_read_break(m: &mut M, _a: [u32; 4]) -> Result {
    stop_streams(m);
    ret(0)
}

fn swap(slot: &mut u32, new: u32) -> u32 {
    std::mem::replace(slot, new)
}

fn cd_read_callback(m: &mut M, a: [u32; 4]) -> Result {
    ret(swap(&mut m.sdk.cd.read_cb, a[0]))
}

fn cd_sync_callback(m: &mut M, a: [u32; 4]) -> Result {
    ret(swap(&mut m.sdk.cd.sync_cb, a[0]))
}

fn cd_ready_callback(m: &mut M, a: [u32; 4]) -> Result {
    ret(swap(&mut m.sdk.cd.ready_cb, a[0]))
}

fn cd_data_callback(m: &mut M, a: [u32; 4]) -> Result {
    ret(swap(&mut m.sdk.cd.data_cb, a[0]))
}

fn cd_status(m: &mut M, _a: [u32; 4]) -> Result {
    ret(status(m) as u32)
}

fn cd_mode(m: &mut M, _a: [u32; 4]) -> Result {
    ret(m.drive.mode as u32)
}

fn cd_last_com(m: &mut M, _a: [u32; 4]) -> Result {
    ret(m.drive.last_command as u32)
}

fn zero(_m: &mut M, _a: [u32; 4]) -> Result {
    ret(0)
}

/// `CdlFILE *CdSearchFile(CdlFILE *fp, char *name)`: `{CdlLOC pos; u_long
/// size; char name[16]}`. Reading the directory takes drive time.
fn cd_search_file(m: &mut M, a: [u32; 4]) -> Result {
    let (fp, name_ptr) = (a[0], a[1]);
    let name = String::from_utf8_lossy(&m.mem.cstr(name_ptr, 64)).into_owned();
    let before = m.sdk.cd.sectors;
    let found = cd::find(&mut HostDisc(&mut *m.host), &name);
    let Some(e) = found.filter(|e| !e.dir) else { return ret(0) };
    let msf = cd::lba_to_msf(e.lba);
    m.mem.write(fp, &[msf[0], msf[1], msf[2], 0]);
    m.mem.w32(fp + 4, e.size);
    let mut n = e.name.as_bytes().to_vec();
    n.resize(16, 0);
    m.mem.write(fp + 8, &n);
    // A directory walk is a few sector reads after a seek.
    let reads = (m.sdk.cd.sectors - before).max(3);
    let t = m.clock.now + m.drive.seek_cycles(m.drive.head, e.lba) + reads * m.drive.sector_cycles();
    m.drive.head = e.lba;
    block(Wait::Cycle(t), move |_m: &mut M| Flow::ret(fp))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{call, machine};
    use psxw_abi::BootOptions;

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

    #[test]
    fn read_lands_sector_by_sector() {
        for (timing, instant) in [(0, false), (1, true)] {
            let mut m = machine(disc(), BootOptions { cd_timing: timing, ..Default::default() });
            m.mem.write(0x8001_0000, &cd::lba_to_msf(20));
            call(&mut m, cd_control, [com::SETLOC as u32, 0x8001_0000, 0, 0]);
            let t0 = m.clock.now;
            assert_eq!(call(&mut m, cd_read, [3, 0x8010_0000, 0x80, 0]), 1);
            assert_eq!(call(&mut m, cd_read_sync, [1, 0, 0, 0]), 3);
            assert_eq!(call(&mut m, cd_read_sync, [0, 0, 0, 0]), 0);
            assert_eq!(m.mem.r8(0x8010_0000), 20);
            assert_eq!(m.mem.r8(0x8010_0000 + 2048 * 2), 22);
            assert_eq!(m.mem.r8(0x8010_0000 + 2048 * 3), 0);
            assert!(m.sdk.cd.edc_errors.is_empty());
            let took = m.clock.now - t0;
            if instant {
                assert_eq!(took, 0);
            } else {
                assert!(took >= 3 * psx_devices::clock::CPU_HZ / 150, "{took}");
            }
        }
    }

    #[test]
    fn corrupt_sector_is_recorded() {
        let mut d = disc();
        d[5][0x100] ^= 1;
        let mut m = machine(d, Default::default());
        m.mem.write(0x8001_0000, &cd::lba_to_msf(5));
        call(&mut m, cd_control, [com::SETLOC as u32, 0x8001_0000, 0, 0]);
        call(&mut m, cd_read, [1, 0x8010_0000, 0, 0]);
        call(&mut m, cd_read_sync, [0, 0, 0, 0]);
        assert_eq!(m.sdk.cd.edc_errors.len(), 1);
    }
}
