//! XA-ADPCM (psx-spx "CDROM XA Audio ADPCM Compression"): the CD
//! controller's decoder for Mode2/Form2 audio sectors, and its resampler to
//! the SPU's 44.1 kHz CD input.
//!
//! A sector's 900h data bytes are 18 portions of 128 bytes: a 16-byte
//! header block (four filter/shift headers, each stored twice) and 28 data
//! words. 4-bit sectors hold 8 blocks of 28 samples per portion (4 per
//! channel when stereo), 8-bit sectors 4. The decoder keeps the two
//! previous samples per channel across sectors.
//!
//! The decoding is psx-spx's, which is exact. The resampler is psx-spx's
//! 25-point zigzag interpolation, which it calls "nearly correct":
//! provisional until the XA oracle vectors (M6 task 8) settle it. 18900 Hz
//! samples are fed to it twice each.

/// psx-spx "Pos/neg Tables" (filters 0-3).
const POS: [i32; 4] = [0, 60, 115, 98];
const NEG: [i32; 4] = [0, 0, -52, -55];

/// Raw-sector offsets: the subheader (after sync and header) and the
/// audio data (after the subheader's two copies).
pub const SUBHEADER: usize = 16;
pub const DATA: usize = 24;
pub const PORTIONS: usize = 18;
pub const PORTION: usize = 128;

/// Subheader submode bits.
pub const SM_EOR: u8 = 1 << 0;
pub const SM_AUDIO: u8 = 1 << 2;
pub const SM_FORM2: u8 = 1 << 5;
pub const SM_REALTIME: u8 = 1 << 6;
pub const SM_EOF: u8 = 1 << 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Subheader {
    pub file: u8,
    pub channel: u8,
    pub submode: u8,
    pub coding: Coding,
}

/// The subheader's coding info byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coding(pub u8);

impl Coding {
    pub fn stereo(self) -> bool {
        self.0 & 3 == 1
    }
    pub fn half_rate(self) -> bool {
        (self.0 >> 2) & 3 == 1
    }
    pub fn eight_bit(self) -> bool {
        (self.0 >> 4) & 3 == 1
    }
    pub fn emphasis(self) -> bool {
        self.0 & 0x40 != 0
    }
}

impl Subheader {
    pub fn parse(raw: &[u8]) -> Subheader {
        let s = &raw[SUBHEADER..SUBHEADER + 4];
        Subheader { file: s[0], channel: s[1], submode: s[2], coding: Coding(s[3]) }
    }

    /// A Form2 audio sector: what the decoder plays.
    pub fn is_audio(&self) -> bool {
        self.submode & (SM_AUDIO | SM_FORM2) == SM_AUDIO | SM_FORM2
    }
}

/// One sector's samples at its own rate: `left` holds mono sectors' too.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Samples {
    pub left: Vec<i16>,
    pub right: Vec<i16>,
    pub stereo: bool,
    pub half_rate: bool,
}

/// The decoder's history: the two previous samples per channel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Decoder {
    /// (old, older) for left/mono and right.
    pub prev: [(i32, i32); 2],
}

/// A header's shift (13..15 act as 9) and filter (0..3).
fn header(h: u8) -> (u32, usize) {
    let s = (h & 0x0F) as u32;
    (if s > 12 { 9 } else { s }, ((h >> 4) & 3) as usize)
}

impl Decoder {
    /// Decode 28 samples of one block: `raw4` is the sample expanded to 16
    /// bits before the shift (`t << 12` for 4-bit, `t << 8` for 8-bit).
    fn block(&mut self, ch: usize, hdr: u8, raw: impl Iterator<Item = i32>, out: &mut Vec<i16>) {
        let (shift, filter) = header(hdr);
        let (f0, f1) = (POS[filter], NEG[filter]);
        let (mut old, mut older) = self.prev[ch];
        for t in raw {
            let s = (t >> shift) + ((old * f0 + older * f1 + 32) >> 6);
            let s = s.clamp(-0x8000, 0x7FFF);
            out.push(s as i16);
            older = old;
            old = s;
        }
        self.prev[ch] = (old, older);
    }

    /// Decode a raw 2352-byte audio sector.
    pub fn decode(&mut self, raw: &[u8]) -> Samples {
        let coding = Subheader::parse(raw).coding;
        let stereo = coding.stereo();
        let mut out = Samples { stereo, half_rate: coding.half_rate(), ..Default::default() };
        for i in 0..PORTIONS {
            let p = &raw[DATA + i * PORTION..DATA + (i + 1) * PORTION];
            let word = |j: usize| u32::from_le_bytes(p[16 + 4 * j..20 + 4 * j].try_into().unwrap());
            if coding.eight_bit() {
                for blk in 0..4 {
                    let raw = (0..28).map(|j| ((word(j) >> (8 * blk)) as u8 as i8 as i32) << 8);
                    let (ch, dst) = if stereo && blk & 1 == 1 { (1, &mut out.right) } else { (0, &mut out.left) };
                    self.block(ch, p[4 + blk], raw, dst);
                }
            } else {
                for blk in 0..8 {
                    let raw = (0..28).map(|j| ((((word(j) >> (4 * blk)) & 0xF) as i32) << 28) >> 16);
                    let (ch, dst) = if stereo && blk & 1 == 1 { (1, &mut out.right) } else { (0, &mut out.left) };
                    self.block(ch, p[4 + blk], raw, dst);
                }
            }
        }
        out
    }
}

/// psx-spx's zigzag tables: `ZIGZAG[phase][i - 1]` for i = 1..29.
const ZIGZAG: [[i32; 29]; 7] = {
    const ROWS: [[i32; 7]; 29] = [
        [0, 0, 0, 0, -0x0001, 0x0002, -0x0005],
        [0, 0, 0, -0x0001, 0x0003, -0x0008, 0x0011],
        [0, 0, -0x0001, 0x0003, -0x0008, 0x0010, -0x0023],
        [0, -0x0002, 0x0003, -0x0008, 0x0011, -0x0023, 0x0046],
        [0, 0, -0x0002, 0x0006, -0x0010, 0x002B, -0x0017],
        [-0x0002, 0x0003, -0x0005, 0x0005, 0x000A, 0x001A, -0x0044],
        [0x000A, -0x0013, 0x001F, -0x001B, 0x006B, -0x00EB, 0x015B],
        [-0x0022, 0x003C, -0x004A, 0x00A6, -0x016D, 0x027B, -0x0347],
        [0x0041, -0x004B, 0x00B3, -0x01A8, 0x0350, -0x0548, 0x080E],
        [-0x0054, 0x00A2, -0x0192, 0x0372, -0x0623, 0x0AFA, -0x1249],
        [0x0034, -0x00E3, 0x02B1, -0x05BF, 0x0BCD, -0x16FA, 0x3C07],
        [0x0009, 0x0132, -0x039E, 0x09B8, -0x1780, 0x53E0, 0x53E0],
        [-0x010A, -0x0043, 0x04F8, -0x11B4, 0x6794, 0x3C07, -0x16FA],
        [0x0400, -0x0267, -0x05A6, 0x74BB, 0x234C, -0x1249, 0x0AFA],
        [-0x0A78, 0x0C9D, 0x7939, 0x0C9D, -0x0A78, 0x080E, -0x0548],
        [0x234C, 0x74BB, -0x05A6, -0x0267, 0x0400, -0x0347, 0x027B],
        [0x6794, -0x11B4, 0x04F8, -0x0043, -0x010A, 0x015B, -0x00EB],
        [-0x1780, 0x09B8, -0x039E, 0x0132, 0x0009, -0x0044, 0x001A],
        [0x0BCD, -0x05BF, 0x02B1, -0x00E3, 0x0034, -0x0017, 0x002B],
        [-0x0623, 0x0372, -0x0192, 0x00A2, -0x0054, 0x0046, -0x0023],
        [0x0350, -0x01A8, 0x00B3, -0x004B, 0x0041, -0x0023, 0x0010],
        [-0x016D, 0x00A6, -0x004A, 0x003C, -0x0022, 0x0011, -0x0008],
        [0x006B, -0x001B, 0x001F, -0x0013, 0x000A, -0x0005, 0x0002],
        [0x000A, 0x0005, -0x0005, 0x0003, -0x0001, 0, 0],
        [-0x0010, 0x0006, -0x0002, 0, 0, 0, 0],
        [0x0011, -0x0008, 0x0003, -0x0002, 0x0001, 0, 0],
        [-0x0008, 0x0003, -0x0001, 0, 0, 0, 0],
        [0x0003, -0x0001, 0, 0, 0, 0, 0],
        [-0x0001, 0, 0, 0, 0, 0, 0],
    ];
    let mut t = [[0; 29]; 7];
    let mut i = 0;
    while i < 29 {
        let mut ph = 0;
        while ph < 7 {
            t[ph][i] = ROWS[i][ph];
            ph += 1;
        }
        i += 1;
    }
    t
};

/// One channel's 37800 → 44100 Hz resampler: every sixth input sample
/// emits seven outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resampler {
    ring: [i16; 32],
    p: usize,
    sixstep: u32,
}

impl Default for Resampler {
    fn default() -> Self {
        // psx-spx: the six-step counter powers up random; 6 until the
        // oracle says otherwise.
        Resampler { ring: [0; 32], p: 0, sixstep: 6 }
    }
}

impl Resampler {
    pub fn push(&mut self, sample: i16, out: &mut Vec<i16>) {
        self.ring[self.p & 31] = sample;
        self.p += 1;
        self.sixstep -= 1;
        if self.sixstep == 0 {
            self.sixstep = 6;
            for table in &ZIGZAG {
                let sum: i32 = (1..=29).map(|i| self.ring[(self.p.wrapping_sub(i)) & 31] as i32 * table[i - 1] / 0x8000).sum();
                out.push(sum.clamp(-0x8000, 0x7FFF) as i16);
            }
        }
    }
}

/// A decoded sector at 44.1 kHz, both channels (mono sectors feed both).
pub fn resample(s: &Samples, rs: &mut [Resampler; 2]) -> (Vec<i16>, Vec<i16>) {
    let reps = if s.half_rate { 2 } else { 1 };
    let mut out = (Vec::new(), Vec::new());
    let right = if s.stereo { &s.right } else { &s.left };
    for (src, (r, dst)) in [&s.left, right].into_iter().zip(rs.iter_mut().zip([&mut out.0, &mut out.1])) {
        for &x in src {
            for _ in 0..reps {
                r.push(x, dst);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A raw audio sector with every portion's headers `hdr` and data
    /// words `word`.
    fn sector(coding: u8, hdr: [u8; 8], word: impl Fn(usize, usize) -> u32) -> Vec<u8> {
        let mut raw = vec![0u8; 2352];
        raw[SUBHEADER..SUBHEADER + 4].copy_from_slice(&[1, 2, SM_AUDIO | SM_FORM2 | SM_REALTIME, coding]);
        raw[SUBHEADER + 4..SUBHEADER + 8].copy_from_slice(&[1, 2, SM_AUDIO | SM_FORM2 | SM_REALTIME, coding]);
        for i in 0..PORTIONS {
            let p = DATA + i * PORTION;
            raw[p..p + 4].copy_from_slice(&hdr[..4]);
            raw[p + 4..p + 8].copy_from_slice(&hdr[..4]);
            raw[p + 8..p + 12].copy_from_slice(&hdr[4..]);
            raw[p + 12..p + 16].copy_from_slice(&hdr[4..]);
            for j in 0..28 {
                raw[p + 16 + 4 * j..p + 20 + 4 * j].copy_from_slice(&word(i, j).to_le_bytes());
            }
        }
        raw
    }

    #[test]
    fn subheader_and_coding() {
        let raw = sector(0x15, [0; 8], |_, _| 0);
        let s = Subheader::parse(&raw);
        assert!(s.is_audio());
        assert_eq!((s.file, s.channel), (1, 2));
        assert!(s.coding.stereo() && s.coding.half_rate() && s.coding.eight_bit() && !s.coding.emphasis());
    }

    #[test]
    fn mono_4bit_blocks_in_order_with_shift_and_clamp() {
        // Filter 0: each sample is the nibble shifted. Block k's nibble is k
        // (blocks 0..7 = nibbles 0..7 of every word), shift 12 - k.
        let hdr = [0x0C, 0x0B, 0x0A, 0x09, 0x08, 0x07, 0x06, 0x05];
        let word = |_: usize, _: usize| 0x7654_3210;
        let s = Decoder::default().decode(&sector(0x00, hdr, word));
        assert_eq!(s.left.len(), PORTIONS * 8 * 28);
        assert!(s.right.is_empty());
        for blk in 0..8 {
            let want = ((blk as i32) << 12) >> (12 - blk);
            assert!(s.left[blk * 28..(blk + 1) * 28].iter().all(|&x| x as i32 == want), "block {blk}");
        }
        // Shift 13..15 act as 9; the nibble 8 is -8.
        let s = Decoder::default().decode(&sector(0x00, [0x0F; 8], |_, _| 0x8888_8888));
        assert_eq!(s.left[0] as i32, (-8 << 12) >> 9);
    }

    #[test]
    fn filters_follow_psx_spx_rounding() {
        // Filter 1 (60/64) after one sample of 0x1000 (shift 0, nibble 1):
        // the rest decay by (old*60 + 32) >> 6.
        let hdr = [0x10; 8];
        let word = |i: usize, j: usize| if i == 0 && j == 0 { 0x0000_0001 } else { 0 };
        let s = Decoder::default().decode(&sector(0x00, hdr, word));
        let mut old = 0x1000;
        assert_eq!(s.left[0], 0x1000);
        for &x in &s.left[1..28] {
            old = (old * 60 + 32) >> 6;
            assert_eq!(x as i32, old);
        }
    }

    #[test]
    fn stereo_alternates_channels_and_keeps_history() {
        let hdr = [0x0C, 0x00, 0x0C, 0x00, 0x0C, 0x00, 0x0C, 0x00];
        let s = Decoder::default().decode(&sector(0x01, hdr, |_, _| 0x7171_7171));
        assert_eq!((s.left.len(), s.right.len()), (PORTIONS * 4 * 28, PORTIONS * 4 * 28));
        assert!(s.left.iter().all(|&x| x == 1), "nibble 1, shift 12");
        assert!(s.right.iter().all(|&x| x == 0x7000), "nibble 7, shift 0");
        let mut d = Decoder::default();
        d.decode(&sector(0x01, [0x1C, 0x10, 0x1C, 0x10, 0x1C, 0x10, 0x1C, 0x10], |_, _| 0x7171_7171));
        assert_ne!(d.prev[0], (0, 0));
        assert_ne!(d.prev[1], (0, 0));
    }

    #[test]
    fn eight_bit_uses_four_blocks() {
        let s = Decoder::default().decode(&sector(0x10, [0x08, 0x08, 0x08, 0x08, 0, 0, 0, 0], |_, _| 0x8001_7F02));
        assert_eq!(s.left.len(), PORTIONS * 4 * 28);
        assert_eq!(&s.left[..1], &[2]);
        assert_eq!(s.left[28], 0x7F);
        assert_eq!(s.left[56], 1);
        assert_eq!(s.left[84], -128);
    }

    #[test]
    fn resampler_rate_and_dc() {
        // 6 in, 7 out. A constant settles, per phase, to the table's
        // truncated per-term sum (the table's DC gain is about 0.91, as
        // psx-spx gives it).
        let mut r = Resampler::default();
        let mut out = Vec::new();
        for _ in 0..600 {
            r.push(0x4000, &mut out);
        }
        assert_eq!(out.len(), 700);
        for (ph, table) in ZIGZAG.iter().enumerate() {
            let want: i32 = table.iter().map(|&k| 0x4000 * k / 0x8000).sum();
            assert_eq!(out[693 + ph] as i32, want, "phase {ph}");
            assert!((0x3900..0x3B00).contains(&want), "{want:#x}");
        }
        // A sector is a multiple of six samples at either rate.
        let s = Samples { left: vec![0; 4032], right: vec![], stereo: false, half_rate: true };
        let (l, rt) = resample(&s, &mut Default::default());
        assert_eq!((l.len(), rt.len()), (4032 * 2 * 7 / 6, 4032 * 2 * 7 / 6));
    }
}
