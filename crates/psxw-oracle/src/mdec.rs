//! The MDEC (M13 task 14, D58): decode vectors and psx-wasm's side of the
//! check.
//!
//! **A vector** is what libpress sends from a reset (OP_MDEC_RUN): the
//! quantization tables, the IDCT's scale table, and a decode command with
//! run-length-coded macroblocks, in 4-, 8-, 15- or 24-bit output, signed
//! or not, with or without bit 15 set. Most use libpress's own tables
//! (`DecDCTReset`: the MPEG-1 intra matrix with DC 2, and the DCT basis ×
//! 2^16) and seeded random, well-formed blocks: a DC term, a few AC terms,
//! an end code. Then edge cases: q_scale 0, zeros in the quant table,
//! coefficients that clamp, blocks that fill all 64 coefficients or
//! overrun them with a run, padding between macroblocks, and random scale
//! tables whose IDCT overflows its intermediate precision. And isolating
//! cases, each answering one question on its own: a lone DC, a lone AC
//! term at scan index 1, 2, 3, 5 or 9 (orientation and rounding), luma
//! DCs only (where each Y block goes), a chroma AC term (where chroma
//! samples go), and flat Cr and Cb (the colour conversion).
//!
//! **The check** sends the same words to psx-devices' MDEC and compares
//! the decoded words byte for byte.

use crate::commands::DeviceUnderTest;
use crate::store::Store;
use crate::vector::{MdecCmd, MdecRun, Vector, VectorId};
use anyhow::{Result, bail, ensure};
use psx_devices::mdec::Mdec;

pub const SET: &str = "synthetic-mdec";
const SEED: u64 = 0x5053_5857_4D44_0001;

/// libpress's default quantization table (`DecDCTReset`), in zigzag
/// order: the MPEG-1 intra matrix with its DC entry 2.
pub const QUANT: [u8; 64] = [
    2, 16, 16, 19, 16, 19, 22, 22, 22, 22, 22, 22, 26, 24, 26, 27, 27, 27, 26, 26, 26, 26, 27, 27, 27, 29, 29, 29, 34, 34, 34, 29,
    29, 29, 27, 27, 29, 29, 32, 32, 34, 34, 37, 38, 37, 35, 35, 34, 35, 38, 38, 40, 40, 40, 48, 48, 46, 46, 56, 56, 58, 69, 69, 83,
];

/// libpress's IDCT scale table: row i is the DCT basis function i × 2^16,
/// rounded down (row 0 5A82h, row 1 7D8Ah 6A6Dh 471Ch 18F8h E707h …).
pub fn scale_table() -> [i16; 64] {
    let mut t = [0i16; 64];
    for i in 0..8 {
        for j in 0..8 {
            let c = if i == 0 { 1.0 / 8f64.sqrt() } else { 0.5 };
            let v = c * (((2 * j + 1) as f64 * i as f64 * std::f64::consts::PI) / 16.0).cos();
            t[i * 8 + j] = (v * 65536.0).floor().clamp(-32768.0, 32767.0) as i16;
        }
    }
    t
}

/// Output depth (command bits 27-28).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Depth {
    Bits4 = 0,
    Bits8 = 1,
    Bits24 = 2,
    Bits15 = 3,
}

impl Depth {
    fn color(self) -> bool {
        matches!(self, Depth::Bits24 | Depth::Bits15)
    }
    /// Blocks per unit of output (a colour macroblock is six).
    fn blocks(self) -> usize {
        if self.color() { 6 } else { 1 }
    }
    /// Decoded words per unit.
    fn words(self) -> u32 {
        match self {
            Depth::Bits4 => 8,
            Depth::Bits8 => 16,
            Depth::Bits24 => 192,
            Depth::Bits15 => 128,
        }
    }
}

pub fn quant_cmd(luma: &[u8; 64], chroma: Option<&[u8; 64]>) -> MdecCmd {
    let mut bytes = luma.to_vec();
    if let Some(c) = chroma {
        bytes.extend_from_slice(c);
    }
    let mut words = vec![0x4000_0000 | chroma.is_some() as u32];
    words.extend(bytes.chunks(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])));
    MdecCmd { words, out: 0 }
}

pub fn scale_cmd(t: &[i16; 64]) -> MdecCmd {
    let mut words = vec![0x6000_0000];
    words.extend(t.chunks(2).map(|p| p[0] as u16 as u32 | (p[1] as u16 as u32) << 16));
    MdecCmd { words, out: 0 }
}

/// A decode command over `halfwords` (padded with end codes to a multiple
/// of 32 words, as libpress pads), reading `units` units of output.
pub fn decode_cmd(depth: Depth, signed: bool, bit15: bool, mut halfwords: Vec<u16>, units: usize) -> MdecCmd {
    while !halfwords.len().is_multiple_of(64) {
        halfwords.push(0xFE00);
    }
    let n = (halfwords.len() / 2) as u32;
    let mut words = vec![0x2000_0000 | (depth as u32) << 27 | (signed as u32) << 26 | (bit15 as u32) << 25 | n];
    words.extend(halfwords.chunks(2).map(|p| p[0] as u32 | (p[1] as u32) << 16));
    MdecCmd { words, out: depth.words() * units as u32 }
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
    /// A signed value in -m..=m.
    fn signed(&mut self, m: i32) -> i32 {
        self.below(2 * m as u32 + 1) as i32 - m
    }
}

fn hw(run_or_q: u32, level: i32) -> u16 {
    ((run_or_q & 0x3F) << 10) as u16 | (level as u16 & 0x3FF)
}

/// How a generated block looks.
#[derive(Clone, Copy)]
struct Style {
    q_scale: u32,
    /// Largest AC level.
    level: i32,
    /// AC terms.
    terms: u32,
    /// Largest run.
    run: u32,
    /// Largest DC magnitude.
    dc: i32,
}

const TYPICAL: Style = Style { q_scale: 8, level: 24, terms: 6, run: 5, dc: 200 };

fn block(rng: &mut Rng, s: Style, out: &mut Vec<u16>) {
    let q = if s.q_scale == 0 { 0 } else { 1 + rng.below(s.q_scale) };
    out.push(hw(q, rng.signed(s.dc)));
    let mut k = 0;
    for _ in 0..rng.below(s.terms + 1) {
        let run = rng.below(s.run + 1);
        if k + run + 1 > 63 {
            break;
        }
        k += run + 1;
        let mut level = rng.signed(s.level);
        if level == 0 {
            level = 1;
        }
        out.push(hw(run, level));
    }
    out.push(0xFE00);
}

fn tables(quant: &[u8; 64], chroma: &[u8; 64], scale: &[i16; 64]) -> Vec<MdecCmd> {
    vec![quant_cmd(quant, Some(chroma)), scale_cmd(scale)]
}

/// Every vector, by name.
pub fn generate() -> Vec<(String, Vector)> {
    let mut rng = Rng(SEED);
    let mut out = Vec::new();
    let scale = scale_table();
    let modes: [(Depth, bool, bool); 6] = [
        (Depth::Bits15, false, false),
        (Depth::Bits15, true, true),
        (Depth::Bits24, false, false),
        (Depth::Bits24, true, false),
        (Depth::Bits8, false, false),
        (Depth::Bits4, true, false),
    ];
    // Seeded macroblocks with libpress's tables, in every mode.
    for &(depth, signed, bit15) in &modes {
        for i in 0..4 {
            let units = if depth.color() { 1 + i } else { 4 * (1 + i) };
            let mut h = Vec::new();
            for _ in 0..units * depth.blocks() {
                block(&mut rng, TYPICAL, &mut h);
            }
            let mut cmds = tables(&QUANT, &QUANT, &scale);
            cmds.push(decode_cmd(depth, signed, bit15, h, units));
            out.push((format!("{depth:?}-{}{}-{i}", if signed { "s" } else { "u" }, if bit15 { "b15" } else { "" }), cmds));
        }
    }
    // Random quant tables, luma and chroma apart, and livelier blocks.
    for i in 0..4 {
        let mut q = [0u8; 64];
        let mut c = [0u8; 64];
        for v in q.iter_mut().chain(c.iter_mut()) {
            *v = 1 + rng.below(255) as u8;
        }
        let depth = if i % 2 == 0 { Depth::Bits24 } else { Depth::Bits15 };
        let mut h = Vec::new();
        for _ in 0..2 * 6 {
            block(&mut rng, Style { q_scale: 63, level: 200, terms: 12, run: 8, dc: 511 }, &mut h);
        }
        let mut cmds = tables(&q, &c, &scale);
        cmds.push(decode_cmd(depth, i >= 2, false, h, 2));
        out.push((format!("random-quant-{i}"), cmds));
    }
    // q_scale 0: every AC coefficient takes the q = 0 path.
    {
        let mut h = Vec::new();
        for _ in 0..6 {
            block(&mut rng, Style { q_scale: 0, ..TYPICAL }, &mut h);
        }
        let mut cmds = tables(&QUANT, &QUANT, &scale);
        cmds.push(decode_cmd(Depth::Bits24, false, false, h, 1));
        out.push(("qscale-0".into(), cmds));
    }
    // Zeros in the quant table, DC included.
    {
        let mut q = QUANT;
        for i in (0..64).step_by(3) {
            q[i] = 0;
        }
        let mut h = Vec::new();
        for _ in 0..6 {
            block(&mut rng, Style { terms: 20, run: 2, ..TYPICAL }, &mut h);
        }
        let mut cmds = tables(&q, &q, &scale);
        cmds.push(decode_cmd(Depth::Bits15, false, true, h, 1));
        out.push(("quant-zeros".into(), cmds));
    }
    // Coefficients that clamp: extreme levels, q_scale 63, quant 255.
    {
        let q = [255u8; 64];
        let mut h = Vec::new();
        for b in 0..12 {
            let sign = if b % 2 == 0 { 1 } else { -1 };
            h.push(hw(63, 511 * sign));
            for k in 0..8 {
                h.push(hw(0, if k % 2 == 0 { -512 } else { 511 }));
            }
            h.push(0xFE00);
        }
        let mut cmds = tables(&q, &q, &scale);
        cmds.push(decode_cmd(Depth::Bits24, false, false, h, 2));
        out.push(("clamp".into(), cmds));
    }
    // Blocks that fill all 64 coefficients with no end code, and a run
    // that overruns the block; then padding between macroblocks.
    {
        let mut h = Vec::new();
        for b in 0..6 {
            h.push(hw(4, rng.signed(100)));
            if b % 2 == 0 {
                for _ in 0..63 {
                    h.push(hw(0, rng.signed(6)));
                }
            } else {
                h.push(hw(30, 5));
                h.push(hw(40, 7)); // past coefficient 63: the block ends
            }
        }
        h.extend([0xFE00; 7]);
        for _ in 0..6 {
            block(&mut rng, TYPICAL, &mut h);
            h.push(0xFE00);
        }
        let mut cmds = tables(&QUANT, &QUANT, &scale);
        cmds.push(decode_cmd(Depth::Bits15, false, false, h, 2));
        out.push(("full-overrun-padding".into(), cmds));
    }
    // Monochrome edge cases: 8-bit blocks with padding, 4-bit unsigned.
    {
        let mut h = Vec::new();
        for _ in 0..8 {
            h.extend([0xFE00; 3]);
            block(&mut rng, Style { level: 120, terms: 10, ..TYPICAL }, &mut h);
        }
        let mut cmds = tables(&QUANT, &QUANT, &scale);
        cmds.push(decode_cmd(Depth::Bits8, true, false, h.clone(), 8));
        out.push(("mono8-padding".into(), cmds));
        let mut cmds = tables(&QUANT, &QUANT, &scale);
        cmds.push(decode_cmd(Depth::Bits4, false, false, h, 8));
        out.push(("mono4-padding".into(), cmds));
    }
    // Random scale tables: the IDCT's intermediate overflows.
    for i in 0..2 {
        let mut t = [0i16; 64];
        for v in t.iter_mut() {
            *v = rng.next() as i16;
        }
        let mut h = Vec::new();
        for _ in 0..6 {
            block(&mut rng, Style { level: 300, terms: 16, q_scale: 30, ..TYPICAL }, &mut h);
        }
        let mut cmds = tables(&QUANT, &QUANT, &t);
        cmds.push(decode_cmd(if i == 0 { Depth::Bits24 } else { Depth::Bits15 }, false, false, h, 1));
        out.push((format!("random-scale-{i}"), cmds));
    }
    // Two decodes in a row, and luma-only quant tables (command bit 0
    // clear) for a monochrome decode.
    {
        let mut cmds = tables(&QUANT, &QUANT, &scale);
        for depth in [Depth::Bits15, Depth::Bits24] {
            let mut h = Vec::new();
            for _ in 0..6 {
                block(&mut rng, TYPICAL, &mut h);
            }
            cmds.push(decode_cmd(depth, false, false, h, 1));
        }
        out.push(("two-decodes".into(), cmds));
        let mut h = Vec::new();
        for _ in 0..4 {
            block(&mut rng, TYPICAL, &mut h);
        }
        out.push((
            "luma-only-quant".into(),
            vec![quant_cmd(&QUANT, None), scale_cmd(&scale), decode_cmd(Depth::Bits8, false, false, h, 4)],
        ));
    }
    out.extend(isolating(&scale));
    out.into_iter().map(|(n, cmds)| (n, Vector::MdecRun(MdecRun { cmds }))).collect()
}

/// The isolating cases (fixed, not seeded).
fn isolating(scale: &[i16; 64]) -> Vec<(String, Vec<MdecCmd>)> {
    let t = |decode: MdecCmd| {
        let mut c = tables(&QUANT, &QUANT, scale);
        c.push(decode);
        c
    };
    let mut out = Vec::new();
    out.push(("lone-dc".into(), t(decode_cmd(Depth::Bits8, true, false, vec![hw(1, 20), 0xFE00, hw(1, -7), 0xFE00], 2))));
    for k in [1u32, 2, 3, 5, 9] {
        let h = vec![hw(1, 0), hw(k - 1, 30), 0xFE00, hw(1, 0), hw(k - 1, -30), 0xFE00];
        out.push((format!("lone-ac{k}"), t(decode_cmd(Depth::Bits8, true, false, h, 2))));
    }
    let mut h = vec![hw(1, 0), 0xFE00, hw(1, 0), 0xFE00];
    for d in [10, 20, 30, 40] {
        h.extend([hw(1, d), 0xFE00]);
    }
    out.push(("luma-dcs".into(), t(decode_cmd(Depth::Bits24, true, false, h, 1))));
    let mut h = vec![hw(1, 0), hw(0, 40), 0xFE00, hw(1, 0), 0xFE00];
    for _ in 0..4 {
        h.extend([hw(1, 0), 0xFE00]);
    }
    out.push(("chroma-ac".into(), t(decode_cmd(Depth::Bits24, true, false, h, 1))));
    for (cr, cb) in [(20, 0), (0, 20), (-25, 13)] {
        let mut h = vec![hw(1, cr), 0xFE00, hw(1, cb), 0xFE00];
        for _ in 0..4 {
            h.extend([hw(1, 10), 0xFE00]);
        }
        out.push((format!("flat-cr{cr}-cb{cb}"), t(decode_cmd(Depth::Bits24, true, false, h, 1))));
    }
    out
}

/// Store the vectors as the `synthetic-mdec` set.
pub fn install(store: &Store) -> Result<Vec<VectorId>> {
    let ids = generate().iter().map(|(_, v)| store.put_vector(v)).collect::<Result<Vec<_>>>()?;
    store.replace_set(SET, ids.iter().copied())?;
    Ok(ids)
}

/// psx-devices' MDEC through the vector's commands, as OP_MDEC_RUN sends
/// them: reset, DMA requests on, each command's words in, its output out.
pub fn run(m: &MdecRun) -> Result<Vec<u8>> {
    let mut d = Mdec::default();
    d.control(0x8000_0000);
    d.control(0x6000_0000);
    let mut out = Vec::new();
    for (i, c) in m.cmds.iter().enumerate() {
        for w in &c.words {
            d.write(*w);
        }
        ensure!(!d.busy(), "command {i} ({:#010x}) still wants parameters", c.words[0]);
        ensure!(
            d.available() >= c.out as usize,
            "command {i} ({:#010x}) decoded {} words; DMA1 reads {}",
            c.words[0],
            d.available(),
            c.out
        );
        for _ in 0..c.out {
            out.extend(d.read().to_le_bytes());
        }
    }
    Ok(out)
}

/// psx-devices' MDEC as the oracle's `mdec` device.
pub struct Device;

impl DeviceUnderTest for Device {
    fn device(&self) -> &str {
        "mdec"
    }

    fn name(&self) -> String {
        "psx-devices mdec".into()
    }

    fn run(&mut self, v: &Vector, _: &Store) -> Result<Vec<u8>> {
        match v {
            Vector::MdecRun(m) => run(m),
            other => bail!("the mdec device cannot run a {} vector", other.kind()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_table_is_libpress() {
        let t = scale_table();
        assert_eq!(t[..8], [0x5A82; 8]);
        let row1: Vec<u16> = t[8..16].iter().map(|&v| v as u16).collect();
        assert_eq!(row1, [0x7D8A, 0x6A6D, 0x471C, 0x18F8, 0xE707, 0xB8E3, 0x9592, 0x8275]);
        let row2: Vec<u16> = t[16..24].iter().map(|&v| v as u16).collect();
        assert_eq!(row2, [0x7641, 0x30FB, 0xCF04, 0x89BE, 0x89BE, 0xCF04, 0x30FB, 0x7641]);
        assert_eq!(t[32] as u16, 0x5A82);
        assert_eq!(t[33] as u16, 0xA57D);
    }

    #[test]
    fn vectors_decode_on_psx_devices() {
        let dir = std::env::temp_dir().join(format!("psxw-oracle-mdec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::new(&dir);
        let all = generate();
        assert!(all.len() >= 45);
        for (name, v) in &all {
            v.check().unwrap();
            crate::script::compile(v, &store).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            let got = Device.run(v, &store).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            assert_eq!(got.len(), v.result_len(), "{name}");
            assert!(got.iter().any(|&b| b != 0), "{name}: all zero");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
