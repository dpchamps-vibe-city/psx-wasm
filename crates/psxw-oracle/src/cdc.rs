//! The CD controller's commands (M13 task 14, D58): the data file on the
//! runner disc, the vectors that drive the controller, and psx-wasm's side
//! of the check.
//!
//! **The test file** (`CDTEST.BIN`) is generated here, deterministically,
//! and written onto the runner disc by `psxw oracle images build`; mkpsxiso
//! makes Mode 2 Form 1 sectors of it, with their EDC and ECC.
//!
//! **A vector** is a command sequence run by OP_CD_CMDS: status commands
//! (Getstat, Setmode, Setfilter, Getparam, GetTN, GetTD, Test 20h, GetID),
//! seeks (Setloc, SeekL, GetlocP), and reads (Setloc, ReadN or ReadS for a
//! few sectors in 2048- or 2340-byte mode at single or double speed, Pause,
//! GetlocL). Each starts with Setmode and Setfilter, so nothing an earlier
//! vector left (the XA vectors' mode and filter) shows. It carries the raw
//! sectors around the test file, read from the disc image when the vector
//! is made, which is the disc psx-wasm's controller reads.
//!
//! **The check** runs psx-devices' controller the way the runner drives
//! the console's: the same port writes and reads, interrupts polled and
//! acknowledged, the data FIFO read before INT1 is acknowledged. The
//! interrupt sequence, every response byte and every sector byte must be
//! equal ([`values`]), with two exceptions that are drive timing (D48)
//! and are recorded but not compared: the times, and the head positions
//! GetlocP and GetlocL report while the drive isn't reading. A paused or
//! seeked drive keeps moving its head (the emulator's GetlocL after a
//! Pause is 8 sectors behind the last one delivered, its GetlocP 2
//! sectors past a SeekL target and advancing), so those positions depend
//! on when they're asked. GetlocL during a read is compared. Sector data
//! is exact by construction: the test file's bytes, in sectors whose EDC
//! the controller checks as it reads them, and whose EDC the check
//! verifies again in the target's 2340-byte reads.

use crate::commands::DeviceUnderTest;
use crate::report::first_difference;
use crate::store::Store;
use crate::vector::{CdCmd, CdCmds, Vector, VectorId};
use crate::wire;
use anyhow::{Context, Result, bail, ensure};
use psx_devices::cd::{self, Disc, RAW};
use psx_devices::cdc::{self, Cdc};
use std::path::Path;

pub const NAME: &str = "CDTEST.BIN";
/// Sectors in the test file.
pub const SECTORS: u32 = 64;
/// Raw sectors a vector carries past the file (the drive reads ahead
/// until a Pause takes effect).
const TAIL: u32 = 16;
pub const SET: &str = "synthetic-cdc";
const SEED: u64 = 0x5053_5857_4344_0001;
/// GetID's region letters for the runner disc: its boot file is named in
/// serial form, `SLUS_…`, which the emulator reads as a North American
/// disc.
pub const REGION: [u8; 4] = *b"SCEA";

/// Setmode values: double speed, and the 2340-byte sector size.
const DOUBLE: u8 = 0x80;
const WHOLE: u8 = 0x20;

pub mod op {
    pub const GETSTAT: u32 = 0x01;
    pub const SETLOC: u32 = 0x02;
    pub const READN: u32 = 0x06;
    pub const PAUSE: u32 = 0x09;
    pub const SETFILTER: u32 = 0x0D;
    pub const SETMODE: u32 = 0x0E;
    pub const GETPARAM: u32 = 0x0F;
    pub const GETLOCL: u32 = 0x10;
    pub const GETLOCP: u32 = 0x11;
    pub const GETTN: u32 = 0x13;
    pub const GETTD: u32 = 0x14;
    pub const SEEKL: u32 = 0x15;
    pub const TEST: u32 = 0x19;
    pub const GETID: u32 = 0x1A;
    pub const READS: u32 = 0x1B;
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        (z ^ (z >> 31)) as u32
    }
    fn below(&mut self, n: u32) -> u32 {
        self.next() % n
    }
}

/// The test file: each sector names itself, then pseudo-random bytes.
pub fn file() -> Vec<u8> {
    let mut rng = Rng(SEED);
    let mut out = Vec::with_capacity(SECTORS as usize * cd::USER);
    for s in 0..SECTORS {
        let mut sector = format!("PSXW CDTEST sector {s:04}\n").into_bytes();
        while sector.len() < cd::USER {
            sector.extend_from_slice(&rng.next().to_le_bytes());
        }
        sector.truncate(cd::USER);
        out.extend(sector);
    }
    out
}

fn cmd(c: u32, params: &[u8], ints: u32, data: u32) -> CdCmd {
    CdCmd { cmd: c, params: params.to_vec(), ints, data }
}

/// Setmode and Setfilter: the state every vector starts from.
fn preamble(mode: u8) -> Vec<CdCmd> {
    vec![cmd(op::SETMODE, &[mode], 1, 0), cmd(op::SETFILTER, &[0, 0], 1, 0)]
}

/// The command sequences, for the test file at `lba`.
pub fn scripts(lba: u32) -> Vec<(String, Vec<CdCmd>)> {
    let mut out = Vec::new();
    let mut status = vec![cmd(op::GETSTAT, &[], 1, 0)];
    status.extend(preamble(DOUBLE));
    status.extend([
        cmd(op::GETPARAM, &[], 1, 0),
        cmd(op::GETTN, &[], 1, 0),
        cmd(op::GETTD, &[0x00], 1, 0),
        cmd(op::GETTD, &[0x01], 1, 0),
        cmd(op::TEST, &[0x20], 1, 0),
        cmd(op::GETID, &[], 2, 0),
        cmd(op::GETSTAT, &[], 1, 0),
    ]);
    out.push(("status".to_string(), status));
    for (name, target) in [("seek-file", lba + 5), ("seek-file-end", lba + SECTORS - 8), ("seek-pvd", 16)] {
        let mut s = preamble(DOUBLE);
        s.extend([
            cmd(op::SETLOC, &cd::lba_to_msf(target), 1, 0),
            cmd(op::SEEKL, &[], 2, 0),
            cmd(op::GETLOCP, &[], 1, 0),
            cmd(op::GETSTAT, &[], 1, 0),
        ]);
        out.push((name.to_string(), s));
    }
    let mut rng = Rng(SEED ^ 0x5245_4144);
    // (mode, read command, data bytes): double and single speed, 2048 and
    // 2340 bytes, ReadN and ReadS (data sectors: ReadS reads them alike).
    let reads: [(u8, u32, u32, &str); 5] = [
        (DOUBLE, op::READN, 0x800, "readn-2048-double"),
        (DOUBLE | WHOLE, op::READN, 0x924, "readn-2340-double"),
        (0, op::READN, 0x800, "readn-2048-single"),
        (WHOLE, op::READN, 0x924, "readn-2340-single"),
        (DOUBLE, op::READS, 0x800, "reads-2048-double"),
    ];
    for (k, &(mode, read, data, name)) in reads.iter().enumerate() {
        for i in 0..2 {
            let count = 2 + rng.below(4);
            let start = lba + rng.below(SECTORS - count - 4);
            let mut s = preamble(mode);
            s.extend([
                cmd(op::SETLOC, &cd::lba_to_msf(start), 1, 0),
                cmd(read, &[], 1 + count, data),
                cmd(op::GETLOCL, &[], 1, 0),
                cmd(op::PAUSE, &[], 2, 0),
                cmd(op::GETSTAT, &[], 1, 0),
            ]);
            if k == 0 && i == 0 {
                // Where the head rests after a pause (recorded, not
                // compared).
                s.extend([cmd(op::GETLOCL, &[], 1, 0), cmd(op::GETLOCP, &[], 1, 0)]);
            }
            out.push((format!("{name}-{i}"), s));
        }
    }
    out
}

/// A disc image (`.bin`, raw sectors).
struct Image(std::fs::File);

impl Disc for Image {
    fn read_raw(&mut self, lba: u32, out: &mut [u8; RAW]) -> bool {
        use std::io::{Read, Seek, SeekFrom};
        self.0.seek(SeekFrom::Start(lba as u64 * RAW as u64)).is_ok() && self.0.read_exact(out).is_ok()
    }
}

/// Store the vectors for the runner disc image `bin` (from its test
/// file's sectors) as the `synthetic-cdc` set. `None` when the disc has no
/// test file (built before it existed).
pub fn install(store: &Store, bin: &Path) -> Result<Option<(u32, Vec<VectorId>)>> {
    let f = std::fs::File::open(bin).with_context(|| format!("opening {}", bin.display()))?;
    let end_lba = (f.metadata()?.len() / RAW as u64) as u32;
    let mut img = Image(f);
    let Some(entry) = cd::find(&mut img, NAME) else { return Ok(None) };
    let want = file();
    ensure!(entry.size as usize == want.len(), "{NAME} on {} is {} bytes, not {}", bin.display(), entry.size, want.len());
    let last = (entry.lba + SECTORS + TAIL).min(end_lba);
    let mut sectors = Vec::with_capacity(((last - entry.lba) as usize) * RAW);
    for lba in entry.lba..last {
        let mut raw = [0u8; RAW];
        ensure!(img.read_raw(lba, &mut raw), "LBA {lba} unreadable on {}", bin.display());
        if lba < entry.lba + SECTORS {
            ensure!(cd::check(&raw, lba) == Ok(cd::SectorKind::Form1), "{NAME} sector at LBA {lba} isn't a valid Form 1 sector");
            let k = (lba - entry.lba) as usize * cd::USER;
            ensure!(cd::user_data(&raw) == &want[k..k + cd::USER], "{NAME} sector at LBA {lba} isn't the generated data");
        }
        sectors.extend_from_slice(&raw);
    }
    let disc = store.put_blob(&sectors)?;
    let mut ids = Vec::new();
    for (_, cmds) in scripts(entry.lba) {
        let v = Vector::CdCmds(CdCmds { disc, first_lba: entry.lba, end_lba, region: REGION, cmds });
        ids.push(store.put_vector(&v)?);
    }
    // Vectors of an earlier disc layout go: they'd read sectors the disc
    // no longer has there.
    for old in store.set(SET)? {
        if !ids.contains(&old) {
            store.remove_vector(&old)?;
        }
    }
    store.replace_set(SET, ids.iter().copied())?;
    Ok(Some((entry.lba, ids)))
}

/// A result's values: everything but the times trailer.
pub fn without_times(result: &[u8]) -> &[u8] {
    let n = result.get(4..8).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    &result[..result.len().saturating_sub(4 * n)]
}

/// Whether a command's response is a head position taken while the drive
/// isn't reading (GetlocP, or GetlocL after a Pause or seek): drive
/// timing, not compared.
fn position_while_idle(cmd: u32, reading: bool) -> bool {
    cmd == op::GETLOCP || (cmd == op::GETLOCL && !reading)
}

/// What the check compares: [`without_times`], with the response bytes of
/// head positions reported while the drive isn't reading zeroed (their
/// interrupt type and response length stay).
pub fn values(c: &CdCmds, result: &[u8]) -> Vec<u8> {
    let mut out = without_times(result).to_vec();
    let mut pos = 8usize;
    let mut reading = false;
    for k in &c.cmds {
        let mask = position_while_idle(k.cmd, reading);
        for _ in 0..k.ints {
            if mask && pos + wire::CD_RECORD_SIZE as usize <= out.len() {
                out[pos + 4..pos + wire::CD_RECORD_SIZE as usize].fill(0);
            }
            pos += (wire::CD_RECORD_SIZE + k.data) as usize;
        }
        match k.cmd {
            op::READN | op::READS => reading = true,
            op::PAUSE | op::SEEKL | 0x08 | 0x0A | 0x16 => reading = false,
            _ => {}
        }
    }
    out
}

/// The vector's sectors as a disc.
struct Sectors<'a> {
    first: u32,
    raw: &'a [u8],
}

impl Disc for Sectors<'_> {
    fn read_raw(&mut self, lba: u32, out: &mut [u8; RAW]) -> bool {
        let Some(i) = lba.checked_sub(self.first) else { return false };
        match self.raw.get(i as usize * RAW..(i as usize + 1) * RAW) {
            Some(s) => {
                out.copy_from_slice(s);
                true
            }
            None => false,
        }
    }
}

/// Three seconds: how long the runner waits for an interrupt.
const WAIT: u64 = psx_devices::clock::CPU_HZ * 3;

/// psx-devices' controller driven as OP_CD_CMDS drives the console's.
/// Returns the result and the controller (for its EDC checks).
pub fn run(c: &CdCmds, store: &Store) -> Result<(Vec<u8>, Cdc)> {
    let raw = store.blob(&c.disc)?;
    ensure!(raw.len() % RAW == 0, "the vector's sectors aren't whole");
    let mut disc = Sectors { first: c.first_lba, raw: &raw };
    let mut cdc = Cdc::default();
    cdc.tracks = vec![0];
    cdc.end = c.end_lba;
    cdc.region = c.region;
    let base = cdc::BASE;
    let mut now = 0u64;
    let mut out = Vec::new();
    cdc.write(base, 0, now);
    out.extend((cdc.read(base + 3) as u32).to_le_bytes());
    let n_records: u32 = c.cmds.iter().map(|k| k.ints).sum();
    out.extend(n_records.to_le_bytes());
    let mut ticks = Vec::new();
    // Advance to the controller's next work, if it has any before `limit`.
    let step = |cdc: &mut Cdc, disc: &mut Sectors, now: &mut u64, limit: u64| -> bool {
        match cdc.next_due() {
            Some(t) if t <= limit => {
                *now = (*now).max(t);
                cdc.run(*now, disc);
                true
            }
            _ => false,
        }
    };
    for k in &c.cmds {
        let limit = now + WAIT;
        while cdc.read(base) & 0x80 != 0 {
            ensure!(step(&mut cdc, &mut disc, &mut now, limit), "the controller stays busy before command {:#04x}", k.cmd);
        }
        cdc.write(base, 1, now);
        cdc.write(base + 3, 0x1F, now);
        for _ in 0..32 {
            if cdc.read(base) & 0x20 == 0 {
                break;
            }
            cdc.read(base + 1);
        }
        cdc.write(base, 0, now);
        for &p in &k.params {
            cdc.write(base + 2, p, now);
        }
        let t0 = now;
        cdc.write(base + 1, k.cmd as u8, now);
        let mut stop = false;
        for _ in 0..k.ints {
            let mut rec = vec![0u8; (wire::CD_RECORD_SIZE + k.data) as usize];
            let mut tick = 0u32;
            if !stop {
                cdc.write(base, 1, now);
                let limit = now + WAIT;
                let mut flags = cdc.read(base + 3) & 7;
                while flags == 0 && step(&mut cdc, &mut disc, &mut now, limit) {
                    flags = cdc.read(base + 3) & 7;
                }
                if flags == 0 {
                    rec[..4].copy_from_slice(&wire::CD_TYPE_TIMEOUT.to_le_bytes());
                    stop = true;
                } else {
                    let mut n = 0;
                    while n < 16 && cdc.read(base) & 0x20 != 0 {
                        rec[4 + n] = cdc.read(base + 1);
                        n += 1;
                    }
                    if flags == 1 && k.data > 0 {
                        cdc.write(base, 0, now);
                        cdc.write(base + 3, 0x80, now);
                        for b in &mut rec[wire::CD_RECORD_SIZE as usize..] {
                            *b = cdc.read(base + 2);
                        }
                        cdc.write(base + 3, 0, now);
                    }
                    cdc.write(base, 1, now);
                    cdc.write(base + 3, 0x1F, now);
                    cdc.write(base, 0, now);
                    rec[..4].copy_from_slice(&(flags as u32 | (n as u32) << 8).to_le_bytes());
                    tick = ((now - t0) / wire::CD_TICK_CYCLES as u64) as u32;
                    stop = flags == 5;
                }
            }
            out.extend(rec);
            ticks.push(tick);
        }
    }
    out.extend(ticks.iter().flat_map(|t| t.to_le_bytes()));
    Ok((out, cdc))
}

/// Every 2340-byte sector in a result must carry a valid EDC (the target's
/// data checked on its own, not only against psx-wasm's).
fn check_sectors(c: &CdCmds, result: &[u8]) -> Result<usize> {
    let mut pos = 8usize;
    let mut checked = 0;
    for k in &c.cmds {
        for _ in 0..k.ints {
            let size = (wire::CD_RECORD_SIZE + k.data) as usize;
            let rec = result.get(pos..pos + size).context("result shorter than its records")?;
            if rec[0] == 1 && k.data == 0x924 {
                let mut raw = [0u8; RAW];
                raw[..12].copy_from_slice(&[0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0]);
                raw[12..12 + 0x924].copy_from_slice(&rec[wire::CD_RECORD_SIZE as usize..]);
                let lba = cd::msf_to_lba([raw[12], raw[13], raw[14]]);
                cd::check(&raw, lba).map_err(|e| anyhow::anyhow!("the target's sector: {e}"))?;
                checked += 1;
            }
            pos += size;
        }
    }
    Ok(checked)
}

/// psx-devices' CD controller, as the oracle checks it.
pub struct Device;

impl DeviceUnderTest for Device {
    fn device(&self) -> &str {
        "cdc"
    }

    fn name(&self) -> String {
        "psx-devices cdc".into()
    }

    fn run(&mut self, v: &Vector, store: &Store) -> Result<Vec<u8>> {
        let Vector::CdCmds(c) = v else { bail!("the cdc device cannot run a {} vector", v.kind()) };
        let (out, cdc) = run(c, store)?;
        ensure!(cdc.edc_errors.is_empty(), "sectors failed their EDC: {}", cdc.edc_errors.join("; "));
        Ok(out)
    }

    /// Values exactly; times not at all (D48).
    fn consistent(&mut self, v: &Vector, store: &Store, golden: &[u8]) -> Option<Result<(), String>> {
        let Vector::CdCmds(c) = v else { return Some(Err("not a CD command vector".into())) };
        let got = match self.run(v, store) {
            Ok(r) => r,
            Err(e) => return Some(Err(format!("{e:#}"))),
        };
        if let Err(e) = check_sectors(c, golden) {
            return Some(Err(format!("{e:#}")));
        }
        let (a, b) = (values(c, &got), values(c, golden));
        Some(if a == b { Ok(()) } else { Err(first_difference(&a, &b)) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sector(lba: u32, data: &[u8]) -> [u8; RAW] {
        let mut s = [0u8; RAW];
        s[..12].copy_from_slice(&[0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0]);
        s[12..15].copy_from_slice(&cd::lba_to_msf(lba));
        s[15] = 2;
        s[0x10..0x18].copy_from_slice(&[0, 0, 0x08, 0, 0, 0, 0x08, 0]);
        s[0x18..0x818].copy_from_slice(data);
        let e = cd::edc(&s[0x10..0x818]);
        s[0x818..0x81C].copy_from_slice(&e.to_le_bytes());
        s
    }

    #[test]
    fn scripts_run_on_psx_devices() {
        let dir = std::env::temp_dir().join(format!("psxw-oracle-cdc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::new(&dir);
        let first = 40;
        let f = file();
        let mut raw = Vec::new();
        for s in 0..SECTORS + TAIL {
            let data = f.get(s as usize * cd::USER..(s as usize + 1) * cd::USER).unwrap_or(&[0; cd::USER]);
            raw.extend_from_slice(&sector(first + s, data));
        }
        let disc = store.put_blob(&raw).unwrap();
        for (name, cmds) in scripts(first) {
            let v = Vector::CdCmds(CdCmds { disc, first_lba: first, end_lba: 2000, region: REGION, cmds: cmds.clone() });
            v.check().unwrap();
            crate::script::compile(&v, &store).unwrap();
            let got = Device.run(&v, &store).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            assert_eq!(got.len(), v.result_len(), "{name}");
            assert_eq!(check_sectors(match &v { Vector::CdCmds(c) => c, _ => unreachable!() }, &got).unwrap() > 0, name.contains("2340"));
            // No command timed out, and reads delivered the file's bytes.
            let mut pos = 8;
            for k in &cmds {
                for _ in 0..k.ints {
                    assert_ne!(got[pos], wire::CD_TYPE_TIMEOUT as u8, "{name}: command {:#x} timed out", k.cmd);
                    if got[pos] == 1 && k.data == 0x800 {
                        let d = &got[pos + 20..pos + 20 + 0x800];
                        assert!(f.chunks(cd::USER).any(|s| s == d), "{name}: sector data isn't the file's");
                    }
                    pos += (wire::CD_RECORD_SIZE + k.data) as usize;
                }
            }
            assert_eq!(without_times(&got).len(), got.len() - 4 * cmds.iter().map(|k| k.ints as usize).sum::<usize>());
            if let Vector::CdCmds(c) = &v {
                let vals = values(c, &got);
                let masked = vals.iter().zip(without_times(&got)).filter(|(a, b)| a != b).count();
                let positions = cmds.iter().filter(|k| k.cmd == op::GETLOCP).count();
                assert!(positions == 0 || masked > 0, "{name}: GetlocP's position is left out");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
