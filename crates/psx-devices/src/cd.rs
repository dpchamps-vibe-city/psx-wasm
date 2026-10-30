//! The CD-ROM: sectors from the user's disc image, verified by EDC; ISO9660
//! lookup; and the drive's position, mode and timing, which the libcd
//! adapters drive. Timing is either realistic (seek plus 75 or 150 sectors
//! a second) or instant (D19: no seeks, the same sectors a second); either
//! way it is a function of virtual time.

use crate::clock::CPU_HZ;

pub const RAW: usize = 2352;
pub const USER: usize = 2048;
/// Sector of MSF 00:02:00 (the first data sector) is LBA 0.
pub const PREGAP: u32 = 150;

/// Where sectors come from (the host's disc image).
pub trait Disc {
    /// One raw 2352-byte sector; false if unreadable or past the end.
    fn read_raw(&mut self, lba: u32, out: &mut [u8; RAW]) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectorKind {
    Mode1,
    Form1,
    Form2,
}

const SYNC: [u8; 12] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

fn edc_table() -> &'static [u32; 256] {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut v = i as u32;
            for _ in 0..8 {
                v = (v >> 1) ^ if v & 1 != 0 { 0xD801_8001 } else { 0 };
            }
            *e = v;
        }
        t
    })
}

/// ECMA-130 EDC (CRC-32 over GF(2) with polynomial 0xD8018001, reflected).
pub fn edc(data: &[u8]) -> u32 {
    let t = edc_table();
    data.iter().fold(0u32, |crc, &b| (crc >> 8) ^ t[((crc ^ b as u32) & 0xFF) as usize])
}

/// Validate a raw sector's sync, header and EDC. Mode 2 form 2 sectors may
/// carry no EDC (zero), which is accepted.
pub fn check(raw: &[u8; RAW], lba: u32) -> Result<SectorKind, String> {
    if raw[..12] != SYNC {
        return Err(format!("LBA {lba}: bad sync"));
    }
    let bcd = |b: u8| (b >> 4) as u32 * 10 + (b & 15) as u32;
    let msf = (bcd(raw[12]) * 60 + bcd(raw[13])) * 75 + bcd(raw[14]);
    if msf != lba + PREGAP {
        return Err(format!("LBA {lba}: header address {msf} != {}", lba + PREGAP));
    }
    let stored = |at: usize| u32::from_le_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]]);
    match raw[15] {
        1 => {
            if edc(&raw[0..0x810]) != stored(0x810) {
                return Err(format!("LBA {lba}: mode 1 EDC mismatch"));
            }
            Ok(SectorKind::Mode1)
        }
        2 => {
            if raw[0x10..0x14] != raw[0x14..0x18] {
                return Err(format!("LBA {lba}: subheader copies differ"));
            }
            if raw[0x12] & 0x20 != 0 {
                let s = stored(0x92C);
                if s != 0 && edc(&raw[0x10..0x92C]) != s {
                    return Err(format!("LBA {lba}: form 2 EDC mismatch"));
                }
                Ok(SectorKind::Form2)
            } else {
                if edc(&raw[0x10..0x818]) != stored(0x818) {
                    return Err(format!("LBA {lba}: form 1 EDC mismatch"));
                }
                Ok(SectorKind::Form1)
            }
        }
        m => Err(format!("LBA {lba}: mode {m}")),
    }
}

/// The 2048 user bytes of a data sector.
pub fn user_data(raw: &[u8; RAW]) -> &[u8] {
    if raw[15] == 1 { &raw[0x10..0x810] } else { &raw[0x18..0x818] }
}

pub fn lba_to_msf(lba: u32) -> [u8; 3] {
    let a = lba + PREGAP;
    let bcd = |v: u32| (((v / 10) << 4) | (v % 10)) as u8;
    [bcd(a / 75 / 60), bcd(a / 75 % 60), bcd(a % 75)]
}

pub fn msf_to_lba(msf: [u8; 3]) -> u32 {
    let bcd = |b: u8| (b >> 4) as u32 * 10 + (b & 15) as u32;
    ((bcd(msf[0]) * 60 + bcd(msf[1])) * 75 + bcd(msf[2])).saturating_sub(PREGAP)
}

/// A file or directory in the ISO9660 file system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub lba: u32,
    pub size: u32,
    pub dir: bool,
}

fn user_sector(disc: &mut dyn Disc, lba: u32) -> Option<Vec<u8>> {
    let mut raw = [0u8; RAW];
    if !disc.read_raw(lba, &mut raw) {
        return None;
    }
    Some(user_data(&raw).to_vec())
}

fn records(disc: &mut dyn Disc, dir: &DirEntry) -> Vec<DirEntry> {
    let mut out = Vec::new();
    let sectors = dir.size.div_ceil(USER as u32);
    for s in 0..sectors {
        let Some(data) = user_sector(disc, dir.lba + s) else { break };
        let mut i = 0;
        while i < USER {
            let len = data[i] as usize;
            if len == 0 || i + len > USER {
                break;
            }
            let r = &data[i..i + len];
            let lba = u32::from_le_bytes([r[2], r[3], r[4], r[5]]);
            let size = u32::from_le_bytes([r[10], r[11], r[12], r[13]]);
            let name_len = r[32] as usize;
            let name = String::from_utf8_lossy(&r[33..33 + name_len.min(len - 33)]).into_owned();
            out.push(DirEntry { name, lba, size, dir: r[25] & 2 != 0 });
            i += len;
        }
    }
    out
}

/// The root directory from the primary volume descriptor (LBA 16).
pub fn root(disc: &mut dyn Disc) -> Option<DirEntry> {
    let pvd = user_sector(disc, 16)?;
    if pvd[0] != 1 || &pvd[1..6] != b"CD001" {
        return None;
    }
    let r = &pvd[156..190];
    Some(DirEntry {
        name: String::new(),
        lba: u32::from_le_bytes([r[2], r[3], r[4], r[5]]),
        size: u32::from_le_bytes([r[10], r[11], r[12], r[13]]),
        dir: true,
    })
}

/// A directory's entries, without itself and its parent.
pub fn list(disc: &mut dyn Disc, dir: &DirEntry) -> Vec<DirEntry> {
    records(disc, dir).into_iter().filter(|e| e.name != "\0" && e.name != "\x01").collect()
}

/// Find a file by path (`cdrom:\DIR\FILE.EXT;1`, `\FILE`, `FILE;1`):
/// case-insensitive, version optional.
pub fn find(disc: &mut dyn Disc, path: &str) -> Option<DirEntry> {
    let path = path.trim_start_matches("cdrom:").trim_start_matches("CDROM:");
    let mut cur = root(disc)?;
    let parts: Vec<&str> = path.split(['\\', '/']).filter(|p| !p.is_empty()).collect();
    for (k, part) in parts.iter().enumerate() {
        let want = part.split(';').next().unwrap_or(part).to_ascii_uppercase();
        let last = k + 1 == parts.len();
        let hit = records(disc, &cur).into_iter().find(|e| {
            let n = e.name.split(';').next().unwrap_or(&e.name).trim_end_matches('.').to_ascii_uppercase();
            n == want && e.dir != last
        })?;
        cur = hit;
    }
    Some(cur)
}

/// Read a whole file's user data.
pub fn read_file(disc: &mut dyn Disc, e: &DirEntry) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(e.size as usize);
    for s in 0..e.size.div_ceil(USER as u32) {
        out.extend(user_sector(disc, e.lba + s)?);
    }
    out.truncate(e.size as usize);
    Some(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Timing {
    #[default]
    Realistic,
    /// Seeks take no time. Sectors still come a sector's time apart, and
    /// commands answer after their latency: with no time between sectors
    /// a read would deliver the whole disc within one cycle, and SDK code
    /// relies on both (libcd's `CdRead` sets up for a read after issuing
    /// it, and waits for the Pause that ends it; libds takes a sector in
    /// its DMA callback before the next comes).
    Instant,
}

/// libcd's mode bits (`CdlSetmode`).
pub mod mode {
    pub const DOUBLE_SPEED: u8 = 0x80;
    pub const XA_ADPCM: u8 = 0x40;
    /// Read 2340 bytes (everything after the sync) instead of 2048.
    pub const SIZE_2340: u8 = 0x20;
    pub const FILTER: u8 = 0x08;
}

/// Drive status bits (`CdlStat*`).
pub mod stat {
    pub const ERROR: u8 = 0x01;
    pub const STANDBY: u8 = 0x02;
    pub const READ: u8 = 0x20;
    pub const SEEK: u8 = 0x40;
    pub const PLAY: u8 = 0x80;
}

/// The drive's mechanical state, as libcd sees it.
#[derive(Debug, Clone, Default)]
pub struct Drive {
    pub timing: Timing,
    pub mode: u8,
    /// Target of the last `Setloc` (LBA).
    pub setloc: u32,
    /// Where the head is (LBA of the next sector under it).
    pub head: u32,
    pub status: u8,
    pub last_command: u8,
}

impl Drive {
    /// CPU cycles per sector at the current speed (either timing).
    pub fn sector_cycles(&self) -> u64 {
        if self.mode & mode::DOUBLE_SPEED != 0 { CPU_HZ / 150 } else { CPU_HZ / 75 }
    }

    /// Seek estimate: a fixed settle time plus a stroke proportional to
    /// distance (full stroke ≈ 300 ms). An unverified model until the
    /// hardware oracle (M10) measures the envelope.
    pub fn seek_cycles(&self, from: u32, to: u32) -> u64 {
        if self.timing == Timing::Instant {
            return 0;
        }
        let d = from.abs_diff(to) as u64;
        if d <= 8 {
            return d * self.sector_cycles();
        }
        let full = 330_000u64; // sectors on a full disc
        CPU_HZ / 50 + (CPU_HZ * 3 / 10) * d.min(full) / full
    }

    /// Command acknowledgement latency (either timing).
    pub fn ack_cycles(&self) -> u64 {
        CPU_HZ / 1000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic mode 2 form 1 disc with an ISO9660 file system.
    pub struct Image(pub Vec<[u8; RAW]>);

    impl Disc for Image {
        fn read_raw(&mut self, lba: u32, out: &mut [u8; RAW]) -> bool {
            match self.0.get(lba as usize) {
                Some(s) => {
                    *out = *s;
                    true
                }
                None => false,
            }
        }
    }

    fn sector(lba: u32, data: &[u8]) -> [u8; RAW] {
        let mut s = [0u8; RAW];
        s[..12].copy_from_slice(&SYNC);
        s[12..15].copy_from_slice(&lba_to_msf(lba));
        s[15] = 2;
        s[0x10..0x14].copy_from_slice(&[0, 0, 0x08, 0]);
        s[0x14..0x18].copy_from_slice(&[0, 0, 0x08, 0]);
        s[0x18..0x18 + data.len()].copy_from_slice(data);
        let e = edc(&s[0x10..0x818]);
        s[0x818..0x81C].copy_from_slice(&e.to_le_bytes());
        s
    }

    fn record(name: &str, lba: u32, size: u32, dir: bool) -> Vec<u8> {
        let mut r = vec![0u8; 33 + name.len() + (name.len() + 1) % 2];
        r[0] = r.len() as u8;
        r[2..6].copy_from_slice(&lba.to_le_bytes());
        r[10..14].copy_from_slice(&size.to_le_bytes());
        r[25] = if dir { 2 } else { 0 };
        r[32] = name.len() as u8;
        r[33..33 + name.len()].copy_from_slice(name.as_bytes());
        r
    }

    pub fn image() -> Image {
        let mut sectors: Vec<[u8; RAW]> = (0..24).map(|l| sector(l, &[])).collect();
        let mut pvd = vec![0u8; USER];
        pvd[0] = 1;
        pvd[1..6].copy_from_slice(b"CD001");
        pvd[156..190].copy_from_slice(&record("\0", 18, 2048, true)[..34]);
        sectors[16] = sector(16, &pvd);
        let mut root = Vec::new();
        root.extend(record("\0", 18, 2048, true));
        root.extend(record("\x01", 18, 2048, true));
        root.extend(record("SYSTEM.CNF;1", 20, 11, false));
        root.extend(record("DATA", 19, 2048, true));
        sectors[18] = sector(18, &root);
        let mut sub = Vec::new();
        sub.extend(record("FILE.BIN;1", 21, 3000, false));
        sectors[19] = sector(19, &sub);
        sectors[20] = sector(20, b"BOOT=x\r\nZZZ");
        sectors[21] = sector(21, &[0xAB; USER]);
        sectors[22] = sector(22, &[0xCD; USER]);
        Image(sectors)
    }

    #[test]
    fn edc_and_msf() {
        let s = sector(5, b"hello");
        assert_eq!(check(&s, 5), Ok(SectorKind::Form1));
        let mut bad = s;
        bad[0x20] ^= 1;
        assert!(check(&bad, 5).is_err());
        assert!(check(&s, 6).is_err());
        assert_eq!(lba_to_msf(0), [0x00, 0x02, 0x00]);
        assert_eq!(msf_to_lba([0x01, 0x23, 0x45]), (60 + 23) * 75 + 45 - 150);
    }

    #[test]
    fn iso9660_lookup() {
        let mut d = image();
        let f = find(&mut d, "cdrom:\\DATA\\FILE.BIN;1").unwrap();
        assert_eq!((f.lba, f.size), (21, 3000));
        let c = find(&mut d, "\\system.cnf").unwrap();
        assert_eq!(read_file(&mut d, &c).unwrap(), b"BOOT=x\r\nZZZ");
        let data = read_file(&mut d, &f).unwrap();
        assert_eq!(data.len(), 3000);
        assert_eq!((data[0], data[2047], data[2048]), (0xAB, 0xAB, 0xCD));
        assert!(find(&mut d, "\\NOPE.BIN").is_none());
    }
}
