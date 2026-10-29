//! The MDEC, the motion decoder (psx-spx "Macroblock Decoder (MDEC)"): at
//! 1F801820h a command or parameter word goes in and decoded data comes
//! out; at 1F801824h the status reads and the control writes. Input comes
//! by DMA0 and output leaves by DMA1. libpress's own code drives it (FMV).
//!
//! Commands: (1) decode run-length-coded macroblocks into 4-, 8-, 15- or
//! 24-bit pixels, (2) set the quantization tables, (3) set the IDCT's
//! scale table. Decoding is instant in virtual time: a macroblock's
//! pixels are ready as soon as its last halfword arrives.
//!
//! The arithmetic follows psx-spx's description of the hardware, with the
//! fixed-point details (coefficient scaling, the IDCT's rounding, the
//! colour conversion's coefficients) as the oracle emulator implements
//! them, marked `oracle:`. Its decode vectors are provisional goldens.

use std::collections::VecDeque;

pub const DATA: u32 = 0x1F80_1820;
pub const STATUS: u32 = 0x1F80_1824;

/// psx-spx "MDEC Decompression": the zigzag order of coefficients.
const ZIGZAG: [usize; 64] = [
    0, 1, 5, 6, 14, 15, 27, 28, 2, 4, 7, 13, 16, 26, 29, 42, 3, 8, 12, 17, 25, 30, 41, 43, 9, 11, 18, 24, 31, 40, 44, 53, 10, 19,
    23, 32, 39, 45, 52, 54, 20, 22, 33, 38, 46, 51, 55, 60, 21, 34, 37, 47, 50, 56, 59, 61, 35, 36, 48, 49, 57, 58, 62, 63,
];

/// Where the `k`th coefficient in scan order goes: psx-spx's `zagzig`
/// (the zigzag table inverted), transposed for the IDCT's orientation
/// (oracle: its table starts 0, 8, 1, 2, 9, 16). The IDCT transposes its
/// result back ([`Mdec::idct`]).
fn scan() -> [usize; 64] {
    let mut out = [0; 64];
    for (raster, &k) in ZIGZAG.iter().enumerate() {
        out[k] = (raster % 8) * 8 + raster / 8;
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cmd {
    /// Decode: output depth (0 4-bit, 1 8-bit, 2 24-bit, 3 15-bit),
    /// signed output, bit 15 of 15-bit pixels.
    Decode { depth: u32, signed: bool, bit15: bool },
    /// Quant tables: luma only, or luma and chroma.
    Quant { color: bool },
    Scale,
}

#[derive(Debug, Clone)]
pub struct Mdec {
    /// The command in progress and its parameter words still to come.
    cmd: Option<(Cmd, u32)>,
    word: u32,
    /// Parameter halfwords not yet consumed (decode) or bytes (tables).
    input: VecDeque<u16>,
    table_bytes: Vec<u8>,
    quant: [[u8; 64]; 2],
    /// The scale table, transposed for the IDCT.
    scale: [i16; 64],
    /// Decoded words for DMA1 or the data port.
    out: VecDeque<u32>,
    /// Bytes of the word being assembled.
    pending: Vec<u8>,
    enable_in: bool,
    enable_out: bool,
    /// Coefficients of the macroblock being decoded: Cr, Cb, Y1..Y4.
    blocks: Vec<[i16; 64]>,
    /// The block within it (status bits 16-18).
    current: u32,
    /// Macroblocks decoded, for the run report.
    pub macroblocks: u64,
}

impl Default for Mdec {
    fn default() -> Self {
        Mdec {
            cmd: None,
            word: 0,
            input: VecDeque::new(),
            table_bytes: Vec::new(),
            quant: [[0; 64]; 2],
            scale: [0; 64],
            out: VecDeque::new(),
            pending: Vec::new(),
            enable_in: false,
            enable_out: false,
            blocks: Vec::new(),
            current: 4,
            macroblocks: 0,
        }
    }
}

fn signed10(v: u16) -> i32 {
    ((v as i32) << 22) >> 22
}

/// oracle: a 9-bit wraparound, then a clamp to a signed byte.
fn clamp_s8(v: i32) -> i32 {
    ((v << 23) >> 23).clamp(-128, 127)
}

impl Mdec {
    pub fn handles(addr: u32) -> bool {
        addr & !3 == DATA || addr & !3 == STATUS
    }

    pub fn status(&self) -> u32 {
        let (depth, signed, bit15) = match self.cmd {
            Some((Cmd::Decode { depth, signed, bit15 }, _)) => (depth, signed, bit15),
            _ => (self.word >> 27 & 3, self.word >> 26 & 1 != 0, self.word >> 25 & 1 != 0),
        };
        let left = self.cmd.map_or(0xFFFF, |(_, n)| n.wrapping_sub(1) & 0xFFFF);
        (self.out.is_empty() as u32) << 31
            | (self.cmd.is_some() as u32) << 29
            | ((self.enable_in && self.cmd.is_some()) as u32) << 28
            | ((self.enable_out && !self.out.is_empty()) as u32) << 27
            | depth << 25
            | (signed as u32) << 24
            | (bit15 as u32) << 23
            | self.current << 16
            | left
    }

    /// Control (1F801824h): reset, and the DMA requests.
    pub fn control(&mut self, v: u32) {
        if v & 0x8000_0000 != 0 {
            self.cmd = None;
            self.input.clear();
            self.table_bytes.clear();
            self.out.clear();
            self.pending.clear();
            self.blocks.clear();
            self.current = 4;
        }
        self.enable_in = v & 0x4000_0000 != 0;
        self.enable_out = v & 0x2000_0000 != 0;
    }

    /// A word at 1F801820h or by DMA0: a command, or its next parameter.
    pub fn write(&mut self, v: u32) {
        let Some((cmd, left)) = self.cmd else {
            self.word = v;
            let n = v & 0xFFFF;
            let cmd = match v >> 29 {
                1 => Cmd::Decode { depth: v >> 27 & 3, signed: v >> 26 & 1 != 0, bit15: v >> 25 & 1 != 0 },
                2 => Cmd::Quant { color: v & 1 != 0 },
                3 => Cmd::Scale,
                _ => return,
            };
            let words = match cmd {
                Cmd::Decode { .. } => n,
                Cmd::Quant { color } => if color { 32 } else { 16 },
                Cmd::Scale => 32,
            };
            if words > 0 {
                self.cmd = Some((cmd, words));
            }
            return;
        };
        match cmd {
            Cmd::Decode { .. } => {
                self.input.push_back(v as u16);
                self.input.push_back((v >> 16) as u16);
                self.decode(cmd);
            }
            _ => self.table_bytes.extend(v.to_le_bytes()),
        }
        if left == 1 {
            self.finish(cmd);
            self.cmd = None;
        } else {
            self.cmd = Some((cmd, left - 1));
        }
    }

    fn finish(&mut self, cmd: Cmd) {
        let b = std::mem::take(&mut self.table_bytes);
        match cmd {
            Cmd::Quant { color } => {
                self.quant[0].copy_from_slice(&b[..64]);
                if color {
                    self.quant[1].copy_from_slice(&b[64..128]);
                }
            }
            Cmd::Scale => {
                for i in 0..64 {
                    let v = i16::from_le_bytes([b[2 * i], b[2 * i + 1]]);
                    // oracle: transposed, and pre-shifted by 3.
                    self.scale[((i & 7) << 3) | ((i >> 3) & 7)] = v >> 3;
                }
            }
            Cmd::Decode { .. } => {
                // Halfwords left over end the command (padding).
                self.input.clear();
                self.blocks.clear();
                self.current = 4;
            }
        }
    }

    /// Decode every whole block the input holds.
    fn decode(&mut self, cmd: Cmd) {
        let Cmd::Decode { depth, signed, bit15 } = cmd else { return };
        let color = depth >= 2;
        loop {
            // Skip padding before a block.
            while self.input.front() == Some(&0xFE00) {
                self.input.pop_front();
            }
            let Some(len) = self.block_len() else { return };
            let chroma = color && self.blocks.len() < 2;
            let block = self.rl_decode(len, chroma);
            self.blocks.push(block);
            let need = if color { 6 } else { 1 };
            self.current = if color { [4, 5, 0, 1, 2, 3][self.blocks.len() % 6] } else { 4 };
            if self.blocks.len() == need {
                let blocks = std::mem::take(&mut self.blocks);
                self.emit(&blocks, depth, signed, bit15);
                self.macroblocks += 1;
            }
        }
    }

    /// Halfwords of the next complete block in the input: its DC term,
    /// then run/level pairs up to the 64th coefficient or an end code.
    fn block_len(&self) -> Option<usize> {
        let mut k = 0;
        for (i, &h) in self.input.iter().enumerate() {
            if i == 0 {
                continue;
            }
            if h == 0xFE00 {
                return Some(i + 1);
            }
            k += ((h >> 10) & 0x3F) as usize + 1;
            if k >= 63 {
                return Some(i + 1);
            }
        }
        None
    }

    /// psx-spx `rl_decode_block` and `idct_core`.
    fn rl_decode(&mut self, len: usize, chroma: bool) -> [i16; 64] {
        let qt = self.quant[chroma as usize];
        let scan = scan();
        let mut coef = [0i32; 64];
        let first = self.input.pop_front().unwrap_or(0);
        let q_scale = ((first >> 10) & 0x3F) as i32;
        // oracle: coefficients carry 4 fraction bits, rounded toward zero
        // by 8 (a half) when nonzero.
        let scaled = |c: i32, q: i32| -> i32 {
            let v = if q != 0 { c * q * 16 + if c != 0 { if c < 0 { 8 } else { -8 } } else { 0 } } else { c * 2 * 16 };
            v.clamp(-0x4000, 0x3FFF)
        };
        coef[scan[0]] = scaled(signed10(first), qt[0] as i32);
        let mut k = 0usize;
        for _ in 1..len {
            let h = self.input.pop_front().unwrap_or(0xFE00);
            if h == 0xFE00 {
                break;
            }
            k += ((h >> 10) & 0x3F) as usize + 1;
            if k > 63 {
                break;
            }
            let q = q_scale * qt[k] as i32;
            let c = signed10(h);
            let v = if q != 0 {
                ((c * q) >> 3) * 16 + if c != 0 { if c < 0 { 8 } else { -8 } } else { 0 }
            } else {
                c * 2 * 16
            };
            coef[scan[k]] = v.clamp(-0x4000, 0x3FFF);
        }
        self.idct(&coef)
    }

    /// oracle: two passes of an 8-point product with the scale table, each
    /// rounding at bit 15, over the transposed coefficients; the result is
    /// transposed back, so scan index 1 is the first horizontal frequency
    /// (Beetle's goldens: a lone coefficient at index 1 varies along each
    /// row, and the rounding falls as after these passes, not as after
    /// passes over untransposed coefficients).
    fn idct(&self, coef: &[i32; 64]) -> [i16; 64] {
        let pass = |src: &[i32; 64]| -> [i32; 64] {
            let mut dst = [0i32; 64];
            for col in 0..8 {
                for x in 0..8 {
                    let sum: i32 = (0..8).map(|u| src[col * 8 + u] * self.scale[x * 8 + u] as i32).sum();
                    dst[x * 8 + col] = (sum + 0x4000) >> 15;
                }
            }
            dst
        };
        let mid = pass(coef);
        let mid = mid.map(|v| v.clamp(-0x8000, 0x7FFF));
        let out = pass(&mid);
        std::array::from_fn(|i| out[(i % 8) * 8 + i / 8] as i16)
    }

    /// One macroblock's pixels into the output.
    fn emit(&mut self, blocks: &[[i16; 64]], depth: u32, signed: bool, bit15: bool) {
        let flip = if signed { 0 } else { 0x80 };
        match depth {
            0 | 1 => {
                // Monochrome: one 8×8 block of luma.
                let y: Vec<i32> = blocks[0].iter().map(|&v| clamp_s8(v as i32)).collect();
                if depth == 1 {
                    let bytes: Vec<u8> = y.iter().map(|&v| (v ^ flip) as u8).collect();
                    self.push_bytes(&bytes);
                } else {
                    // oracle: rounded to 4 bits (+8, saturating), not
                    // truncated.
                    let nibble = |v: i32| ((v + 8).min(127) ^ flip) as u8 >> 4;
                    let nib: Vec<u8> = y.chunks(2).map(|p| nibble(p[0]) | nibble(p[1]) << 4).collect();
                    self.push_bytes(&nib);
                }
            }
            _ => {
                let (cr, cb) = (&blocks[0], &blocks[1]);
                let mut px = [[0i32; 3]; 256];
                for (q, yb) in blocks[2..6].iter().enumerate() {
                    let (bx, by) = ((q & 1) * 8, (q >> 1) * 8);
                    for y in 0..8 {
                        for x in 0..8 {
                            let (px_x, px_y) = (bx + x, by + y);
                            let c = (px_y / 2) * 8 + px_x / 2;
                            let (l, cr, cb) = (clamp_s8(yb[y * 8 + x] as i32), clamp_s8(cr[c] as i32), clamp_s8(cb[c] as i32));
                            // oracle: the colour conversion's coefficients
                            // and masks.
                            let r = clamp_s8(l + ((359 * cr + 0x80) >> 8));
                            let g = clamp_s8(l + ((((-88 * cb) & !0x1F) + ((-183 * cr) & !0x07) + 0x80) >> 8));
                            let b = clamp_s8(l + ((454 * cb + 0x80) >> 8));
                            px[px_y * 16 + px_x] = [r, g, b];
                        }
                    }
                }
                if depth == 2 {
                    let bytes: Vec<u8> = px.iter().flat_map(|p| p.map(|c| (c ^ flip) as u8)).collect();
                    self.push_bytes(&bytes);
                } else {
                    // oracle: each component rounded to 5 bits (+4,
                    // saturating), not truncated.
                    let halves: Vec<u8> = px
                        .iter()
                        .flat_map(|p| {
                            let [r, g, b] = p.map(|c| (((c + 4).min(127) ^ flip) as u8 >> 3) as u16);
                            (r | g << 5 | b << 10 | (bit15 as u16) << 15).to_le_bytes()
                        })
                        .collect();
                    self.push_bytes(&halves);
                }
            }
        }
    }

    fn push_bytes(&mut self, b: &[u8]) {
        self.pending.extend_from_slice(b);
        let whole = self.pending.len() / 4 * 4;
        let words: Vec<u32> = self.pending.drain(..whole).collect::<Vec<u8>>().chunks(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        self.out.extend(words);
    }

    /// Decoded words available (DMA1).
    pub fn available(&self) -> usize {
        self.out.len()
    }

    /// A decode command is waiting for input.
    pub fn busy(&self) -> bool {
        self.cmd.is_some()
    }

    /// The next decoded word (1F801820h, or DMA1).
    pub fn read(&mut self) -> u32 {
        self.out.pop_front().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tables(m: &mut Mdec) {
        // Flat quantization, and the DCT basis libpress uploads
        // (DecDCTReset), as halfwords.
        m.write(0x4000_0001);
        for _ in 0..32 {
            m.write(0x0101_0101);
        }
        let scale: [i16; 64] = {
            let mut t = [0i16; 64];
            for i in 0..8 {
                for j in 0..8 {
                    let c = if i == 0 { 1.0 / 8f64.sqrt() } else { 0.5 };
                    let v = c * (((2 * j + 1) as f64 * i as f64 * std::f64::consts::PI) / 16.0).cos();
                    // libpress's table: the basis × 2^16 (DC row 5A82h).
                    t[i * 8 + j] = (v * 65536.0).round().clamp(-32768.0, 32767.0) as i16;
                }
            }
            t
        };
        m.write(0x6000_0000);
        for p in scale.chunks(2) {
            m.write(p[0] as u16 as u32 | (p[1] as u16 as u32) << 16);
        }
        assert!(!m.busy());
    }

    #[test]
    fn scan_order_is_the_oracles() {
        assert_eq!(scan()[..6], [0, 8, 1, 2, 9, 16]);
    }

    #[test]
    fn a_flat_macroblock_decodes_to_one_colour() {
        let mut m = Mdec::default();
        m.control(0x6000_0000);
        tables(&mut m);
        // Six blocks, each a DC term only (q_scale 1) and an end code: Cr
        // and Cb zero, luma DC 0x40.
        let blocks: [u16; 12] = [0x0400, 0xFE00, 0x0400, 0xFE00, 0x0440, 0xFE00, 0x0440, 0xFE00, 0x0440, 0xFE00, 0x0440, 0xFE00];
        // 15-bit output, bit 15 set.
        m.write(0x3800_0000 | 0x0200_0000 | blocks.len() as u32 / 2);
        assert!(m.busy());
        assert_ne!(m.status() & 1 << 28, 0, "wants input");
        for p in blocks.chunks(2) {
            m.write(p[0] as u32 | (p[1] as u32) << 16);
        }
        assert!(!m.busy());
        assert_eq!(m.macroblocks, 1);
        assert_eq!(m.available(), 128, "16×16 halfwords");
        let first = m.read();
        assert!(m.status() & 1 << 27 != 0);
        let px = first as u16;
        assert_eq!(px >> 15, 1);
        let (r, g, b) = (px & 31, px >> 5 & 31, px >> 10 & 31);
        assert!(r == g && g == b, "grey: {r} {g} {b}");
        assert!(r > 16, "brighter than mid-grey: {r}");
        assert!((1..128).all(|_| m.read() == first), "flat");
    }
}
