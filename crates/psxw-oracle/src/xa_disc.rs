//! XA-ADPCM through the CD controller (M6 task 8): the test file on the
//! runner disc, the vectors that play it, and psx-wasm's side of the check.
//!
//! **The test file** (`XATEST.XA`, file number 1) is generated here,
//! deterministically, and written onto the runner disc by `psxw oracle
//! images build`. It is one region per coding (4- and 8-bit, mono and
//! stereo, 37.8 and 18.9 kHz). Each region interleaves channels at the
//! factor that streams that coding in real time at double speed, so the
//! drive's decoder is never fed faster than it plays.
//! - **Channels 0-3** carry pseudo-random sound groups: every filter and
//!   shift, and in channel 3 of the 4-bit stereo and mono regions also the
//!   reserved shifts 13-15. The other slots hold filler channels that the
//!   filter must skip.
//! - **Each channel's first sector** uses filter 0 in every block, so
//!   decoding from it doesn't depend on the decoder's history.
//!
//! **A vector** plays one channel from its first sector (Setmode C8h:
//! double speed, XA-ADPCM, filter on; Setfilter file 1, the channel). It
//! captures the SPU's CD-left and CD-right buffers 0.8 s after ReadS is
//! acknowledged (OP_CD_XA), when the channel is playing whatever the seek
//! took.
//!
//! **The check** decodes and resamples the same sectors with
//! `psx_devices::xa`. Both 512-sample capture rings must appear in that
//! stream, exactly, as one contiguous window each. When the runner read
//! them is the emulator's timing, not psx-wasm's, so any position is
//! accepted, and so is any of the six phases of the resampler's
//! six-step counter (psx-spx: uninitialized at power-up).

use crate::commands::DeviceUnderTest;
use crate::store::Store;
use crate::vector::{CdXa, Vector, VectorId};
use anyhow::{Context, Result, bail, ensure};
use psx_devices::xa;
use std::collections::HashMap;

/// A Mode 2 sector after its sync and header: subheader (twice), 2324 data
/// bytes, EDC. mkpsxiso's `xa` file type.
pub const SECTOR: usize = 2336;
pub const NAME: &str = "XATEST.XA";
pub const FILE_NUMBER: u8 = 1;
pub const SET: &str = "synthetic-xa";
const SEED: u64 = 0x5053_5857_5841_0001;
/// Channels played per region; the region's other slots are filler.
pub const CHANNELS: u32 = 4;
/// Setmode: double speed, XA-ADPCM, XA filter.
pub const MODE: u32 = 0xC8;
/// The SPU's CD-left and CD-right capture buffers.
pub const CAPTURES: [(u32, u32); 2] = [(0x000, 0x400), (0x400, 0x400)];

/// (name, coding info, interleave, filters): the interleave plays the
/// coding in real time at 150 sectors a second. Without filters, every
/// block uses filter 0, which leaves the resampler as the only arithmetic
/// between the data and the capture.
pub const REGIONS: [(&str, u8, u32, bool); 7] = [
    ("4bit-stereo-37k8-filter0", 0x01, 8, false),
    ("4bit-stereo-37k8", 0x01, 8, true),
    ("4bit-mono-37k8", 0x00, 16, true),
    ("4bit-stereo-18k9", 0x05, 16, true),
    ("4bit-mono-18k9", 0x04, 32, true),
    ("8bit-stereo-37k8", 0x11, 4, true),
    ("8bit-mono-37k8", 0x10, 8, true),
];

/// Seconds of audio each channel holds: more than the drive's start
/// latency (up to about 0.4 s, depending on the seek) plus the capture
/// time.
const SECONDS_PER_CHANNEL: u32 = 12; // tenths
/// When the captures are read, after ReadS: 0.8 s.
const CAPTURE_SAMPLES: u32 = 44_100 * 8 / 10;

/// Sectors each channel of region `i` has.
pub fn sectors_per_channel(i: usize) -> u32 {
    (SECONDS_PER_CHANNEL * 150).div_ceil(10 * REGIONS[i].2)
}

/// Region `i`'s first sector in the file.
pub fn region_start(i: usize) -> u32 {
    (0..i).map(|j| REGIONS[j].2 * sectors_per_channel(j)).sum()
}

/// The end (exclusive) of the region holding file sector `s`.
pub fn region_end(s: u32) -> u32 {
    (1..=REGIONS.len()).map(region_start).find(|&e| e > s).unwrap_or(s)
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
}

fn sector(channel: u8, coding: u8, first: bool, reserved_shifts: bool, rng: &mut Rng) -> [u8; SECTOR] {
    let mut s = [0u8; SECTOR];
    let sub = [FILE_NUMBER, channel, xa::SM_AUDIO | xa::SM_FORM2 | xa::SM_REALTIME, coding];
    s[0..4].copy_from_slice(&sub);
    s[4..8].copy_from_slice(&sub);
    for i in 0..xa::PORTIONS {
        let p = 8 + i * xa::PORTION;
        let mut hdr = [0u8; 8];
        for h in hdr.iter_mut() {
            let shift = if reserved_shifts { rng.next() % 16 } else { rng.next() % 13 } as u8;
            let filter = if first { 0 } else { (rng.next() % 4) as u8 };
            *h = filter << 4 | shift;
        }
        s[p..p + 4].copy_from_slice(&hdr[..4]);
        s[p + 4..p + 8].copy_from_slice(&hdr[..4]);
        s[p + 8..p + 12].copy_from_slice(&hdr[4..]);
        s[p + 12..p + 16].copy_from_slice(&hdr[4..]);
        for j in 0..28 {
            s[p + 16 + 4 * j..p + 20 + 4 * j].copy_from_slice(&rng.next().to_le_bytes());
        }
    }
    let edc = psx_devices::cd::edc(&s[..SECTOR - 4]);
    s[SECTOR - 4..].copy_from_slice(&edc.to_le_bytes());
    s
}

/// The test file: every region's sectors, in order.
pub fn file() -> Vec<u8> {
    let mut rng = Rng(SEED);
    let mut out = Vec::new();
    for (i, &(_, coding, interleave, filters)) in REGIONS.iter().enumerate() {
        for k in 0..interleave * sectors_per_channel(i) {
            let channel = (k % interleave) as u8;
            let filter0 = !filters || k / interleave == 0;
            let reserved = channel == 3 && (i == 1 || i == 2);
            out.extend_from_slice(&sector(channel, coding, filter0, reserved, &mut rng));
        }
    }
    out
}

/// Store the test file and the vectors that play it from `file_lba` (the
/// file's first sector on the target's disc). Returns their ids.
pub fn install(store: &Store, file_lba: u32) -> Result<Vec<VectorId>> {
    let xa = store.put_blob(&file())?;
    let mut ids = Vec::new();
    for i in 0..REGIONS.len() {
        for channel in 0..CHANNELS {
            let v = Vector::CdXa(CdXa {
                xa,
                file_lba,
                start: region_start(i) + channel,
                mode: MODE,
                file: FILE_NUMBER as u32,
                channel,
                samples: CAPTURE_SAMPLES,
                captures: CAPTURES.to_vec(),
            });
            ids.push(store.put_vector(&v)?);
        }
    }
    // Vectors of an earlier test file or disc layout go: they'd play
    // sectors the disc no longer has.
    for old in store.set(SET)? {
        if !ids.contains(&old) {
            store.remove_vector(&old)?;
        }
    }
    store.replace_set(SET, ids.iter().copied())?;
    Ok(ids)
}

/// The 44.1 kHz stream (left, right) the drive plays from sector `start`
/// of `file` to the end of its region, with the filter on (file,
/// channel), the resampler's six-step counter starting at `phase` (1-6).
pub fn stream(file: &[u8], start: u32, filter: (u8, u8), phase: u32) -> (Vec<i16>, Vec<i16>) {
    let end = region_end(start);
    let mut dec = xa::Decoder::default();
    let mut rs = [xa::Resampler::with_phase(phase), xa::Resampler::with_phase(phase)];
    let (mut l, mut r) = (Vec::new(), Vec::new());
    let mut raw = vec![0u8; 16 + SECTOR];
    for s in file.chunks_exact(SECTOR).take(end as usize).skip(start as usize) {
        raw[16..].copy_from_slice(s);
        let sh = xa::Subheader::parse(&raw);
        if !sh.is_audio() || (sh.file, sh.channel) != filter {
            continue;
        }
        let (a, b) = xa::resample(&dec.decode(&raw), &mut rs);
        l.extend(a);
        r.extend(b);
    }
    (l, r)
}

/// Where `ring` (a capture buffer read while it was being written) sits in
/// `stream`: (stream index of its oldest sample, its position in the ring).
pub fn find_ring(stream: &[i16], ring: &[i16]) -> Option<(usize, usize)> {
    const KEY: usize = 8;
    let n = ring.len();
    if stream.len() < n || n < 2 * KEY {
        return None;
    }
    let mut index: HashMap<&[i16], Vec<usize>> = HashMap::new();
    for (i, w) in stream.windows(KEY).enumerate() {
        index.entry(w).or_default().push(i);
    }
    // Anchor on the ring's middle, which is away from any seam.
    for seam in 0..n {
        let mid = n / 2;
        let key: Vec<i16> = (0..KEY).map(|j| ring[(seam + mid + j) % n]).collect();
        for &at in index.get(key.as_slice()).map(Vec::as_slice).unwrap_or(&[]) {
            let Some(k) = at.checked_sub(mid) else { continue };
            if k + n <= stream.len() && (0..n).all(|j| ring[(seam + j) % n] == stream[k + j]) {
                return Some((k, seam));
            }
        }
    }
    None
}

fn samples(b: &[u8]) -> Vec<i16> {
    b.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect()
}

/// psx-wasm's XA path, as the oracle checks it.
pub struct Device;

impl DeviceUnderTest for Device {
    fn device(&self) -> &str {
        "xa"
    }

    fn name(&self) -> String {
        "psx-devices xa".into()
    }

    fn run(&mut self, _v: &Vector, _store: &Store) -> Result<Vec<u8>> {
        bail!("XA captures are checked for consistency, not reproduced")
    }

    fn consistent(&mut self, v: &Vector, store: &Store, golden: &[u8]) -> Option<Result<(), String>> {
        let Vector::CdXa(x) = v else { return Some(Err("not an XA vector".into())) };
        Some(check(x, store, golden).map_err(|e| format!("{e:#}")))
    }
}

fn check(x: &CdXa, store: &Store, golden: &[u8]) -> Result<()> {
    let file = store.blob(&x.xa)?;
    ensure!(x.captures == CAPTURES, "only the CD capture buffers are checked");
    ensure!(golden.len() == 0x800, "{} bytes of captures", golden.len());
    let (left, right) = (samples(&golden[..0x400]), samples(&golden[0x400..]));
    ensure!(left.iter().chain(&right).any(|&s| s != 0), "the captures are silent: the drive played nothing");
    let mut tried = Vec::new();
    for phase in 1..=6 {
        let (l, r) = stream(&file, x.start, (x.file as u8, x.channel as u8), phase);
        match (find_ring(&l, &left), find_ring(&r, &right)) {
            (Some(_), Some(_)) => return Ok(()),
            (a, b) => tried.push(format!("phase {phase}: left {}, right {}", a.is_some(), b.is_some())),
        }
    }
    let (l, _) = stream(&file, x.start, (x.file as u8, x.channel as u8), 6);
    let near = closest(&l, &left);
    bail!("the captures aren't windows of psx-wasm's stream ({}); {near}", tried.join("; "))
}

/// The best match of `ring` against `stream`, for the report.
fn closest(stream: &[i16], ring: &[i16]) -> String {
    let n = ring.len();
    let mut best = (0usize, 0usize, 0usize);
    for seam in (0..n).step_by(8) {
        for k in (0..stream.len().saturating_sub(n)).step_by(1) {
            let hits = (0..n).step_by(16).filter(|&j| ring[(seam + j) % n] == stream[k + j]).count();
            if hits > best.0 {
                best = (hits, k, seam);
            }
        }
    }
    let (_, k, seam) = best;
    let first = (0..n).find(|&j| ring[(seam + j) % n] != stream[k + j]);
    match first {
        Some(j) => format!(
            "closest window at stream {k} (seam {seam}) first differs at {j}: captured {} vs {}",
            ring[(seam + j) % n],
            stream[k + j]
        ),
        None => format!("closest window at stream {k} matches"),
    }
}

/// The file's first sector on a disc image (ISO9660), for `install`.
pub fn file_lba(bin: &std::path::Path) -> Result<u32> {
    struct Image(std::fs::File);
    impl psx_devices::cd::Disc for Image {
        fn read_raw(&mut self, lba: u32, out: &mut [u8; psx_devices::cd::RAW]) -> bool {
            use std::io::{Read, Seek, SeekFrom};
            self.0.seek(SeekFrom::Start(lba as u64 * psx_devices::cd::RAW as u64)).is_ok() && self.0.read_exact(out).is_ok()
        }
    }
    let mut img = Image(std::fs::File::open(bin).with_context(|| format!("opening {}", bin.display()))?);
    let e = psx_devices::cd::find(&mut img, NAME).with_context(|| format!("{NAME} is not on {}", bin.display()))?;
    Ok(e.lba)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_layout() {
        let f = file();
        let total: u32 = (0..REGIONS.len()).map(|i| REGIONS[i].2 * sectors_per_channel(i)).sum();
        assert_eq!(f.len(), total as usize * SECTOR);
        let first = &f[region_start(2) as usize * SECTOR..];
        assert_eq!(&first[..4], &[1, 0, 0x64, 0x00], "4-bit mono region, channel 0");
        let mut raw = vec![0u8; 16 + SECTOR];
        raw[16..].copy_from_slice(&first[..SECTOR]);
        let edc = u32::from_le_bytes(raw[16 + SECTOR - 4..].try_into().unwrap());
        assert_eq!(edc, psx_devices::cd::edc(&raw[16..16 + SECTOR - 4]));
        // Every block of a channel's first sector uses filter 0.
        assert!((0..xa::PORTIONS).all(|i| first[8 + i * xa::PORTION + 4..8 + i * xa::PORTION + 12].iter().all(|h| h >> 4 == 0)));
    }

    #[test]
    fn streams_skip_other_channels() {
        let f = file();
        let (l, r) = stream(&f, region_start(1) + 2, (1, 2), 6);
        // Stereo 37.8 kHz sectors of channel 2: 2016 samples each, 6 in : 7 out.
        let n = sectors_per_channel(1) as usize;
        assert_eq!((l.len(), r.len()), (n * 2016 * 7 / 6, n * 2016 * 7 / 6));
        let (m, _) = stream(&f, region_start(4), (1, 0), 3);
        assert_eq!(m.len(), sectors_per_channel(4) as usize * 4032 * 7 / 3, "18.9 kHz mono: 7 outputs every 3 inputs");
    }

    #[test]
    fn rings_are_found_at_any_seam() {
        let f = file();
        let (l, _) = stream(&f, region_start(0), (1, 0), 4);
        let k = 5000;
        for seam in [0, 1, 255, 511] {
            let mut ring = vec![0i16; 512];
            for j in 0..512 {
                ring[(seam + j) % 512] = l[k + j];
            }
            assert_eq!(find_ring(&l, &ring), Some((k, seam)));
        }
        let mut wrong = l[k..k + 512].to_vec();
        wrong[300] = wrong[300].wrapping_add(1);
        assert_eq!(find_ring(&l, &wrong), None);
    }
}
