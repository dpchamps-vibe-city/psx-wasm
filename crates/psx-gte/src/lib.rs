//! The Geometry Transformation Engine as a fixed-point library with the
//! hardware's register-file API: `mtc2`/`mfc2`/`ctc2`/`cfc2` and
//! `exec(cmd)` on a [`Gte`].
//!
//! The specification is psx-spx, "Geometry Transformation Engine (GTE)":
//! register widths and read-back quirks, the FIFOs, LZCS→LZCR, IRGB/ORGB,
//! every documented command with its `sf`, `lm` and MVMVA `mx`/`v`/`cv`
//! fields (including the far-colour bug and the garbage matrix), the UNR
//! division, and FLAG with bit 31 as the summary of bits 30..23 and 18..13.
//! Where psx-spx leaves a detail open, the device oracle settled it; each
//! such point is recorded in plans/completed/M04-pixels.md (Findings) and marked
//! "oracle:" below.
//!
//! One implementation serves every caller: the `gte_*` macro shims in C,
//! translated COP2 instructions, and Rust libgte adapters. [`Gte`] is
//! `repr(C)` with the hardware's register numbering, so the runtime can keep
//! it in the guest window's runtime-private page ([`Gte::view_mut`]).

#![no_std]

/// The GTE's state: 32 data registers (cop2r0-31) and 32 control registers
/// (cop2r32-63), numbered as the hardware numbers them.
///
/// Each word holds exactly what `mfc2`/`cfc2` returns for that register:
/// 16-bit registers are stored sign- or zero-extended as the hardware reads
/// them back, and the derived registers (SXYP, IRGB, ORGB, LZCR, FLAG bit
/// 31) are kept current. Write through [`Gte::mtc2`]/[`Gte::ctc2`] to keep
/// that invariant; commands decode every operand at its hardware width, so
/// they behave correctly even on raw words.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Gte {
    pub data: [u32; 32],
    pub ctrl: [u32; 32],
}

/// The register file under its earlier name.
pub type GteRegs = Gte;

/// Data register numbers (cop2r0-31).
pub mod dr {
    pub const VXY0: usize = 0;
    pub const VZ0: usize = 1;
    pub const VXY1: usize = 2;
    pub const VZ1: usize = 3;
    pub const VXY2: usize = 4;
    pub const VZ2: usize = 5;
    pub const RGBC: usize = 6;
    pub const OTZ: usize = 7;
    pub const IR0: usize = 8;
    pub const IR1: usize = 9;
    pub const IR2: usize = 10;
    pub const IR3: usize = 11;
    pub const SXY0: usize = 12;
    pub const SXY1: usize = 13;
    pub const SXY2: usize = 14;
    pub const SXYP: usize = 15;
    pub const SZ0: usize = 16;
    pub const SZ1: usize = 17;
    pub const SZ2: usize = 18;
    pub const SZ3: usize = 19;
    pub const RGB0: usize = 20;
    pub const RGB1: usize = 21;
    pub const RGB2: usize = 22;
    pub const RES1: usize = 23;
    pub const MAC0: usize = 24;
    pub const MAC1: usize = 25;
    pub const MAC2: usize = 26;
    pub const MAC3: usize = 27;
    pub const IRGB: usize = 28;
    pub const ORGB: usize = 29;
    pub const LZCS: usize = 30;
    pub const LZCR: usize = 31;
}

/// Control register numbers (cop2r32-63 as cnt0-31).
pub mod cr {
    /// RT11/RT12 .. RT33: words 0..=4.
    pub const RT: usize = 0;
    pub const TRX: usize = 5;
    /// L11/L12 .. L33: words 8..=12.
    pub const LLM: usize = 8;
    pub const RBK: usize = 13;
    /// LR1/LR2 .. LB3: words 16..=20.
    pub const LCM: usize = 16;
    pub const RFC: usize = 21;
    pub const OFX: usize = 24;
    pub const OFY: usize = 25;
    pub const H: usize = 26;
    pub const DQA: usize = 27;
    pub const DQB: usize = 28;
    pub const ZSF3: usize = 29;
    pub const ZSF4: usize = 30;
    pub const FLAG: usize = 31;
}

/// FLAG bits (psx-spx "GTE Saturation").
pub mod flag {
    /// Bit 31: set when any of bits 30..23 or 18..13 is set.
    pub const ERROR: u32 = 1 << 31;
    /// The bits that bit 31 summarises.
    pub const ERROR_SOURCES: u32 = 0x7F87_E000;
    /// Bits software can write (30..12); bits 0..11 always read zero.
    pub const WRITABLE: u32 = 0x7FFF_F000;
    /// MACn larger than 43 bits and positive (n = 1..3): bit 31 - n.
    pub const fn mac_pos(n: usize) -> u32 {
        1 << (31 - n)
    }
    /// MACn larger than 43 bits and negative (n = 1..3): bit 28 - n.
    pub const fn mac_neg(n: usize) -> u32 {
        1 << (28 - n)
    }
    /// IRn saturated (n = 1..3): bit 25 - n.
    pub const fn ir(n: usize) -> u32 {
        1 << (25 - n)
    }
    /// Colour FIFO component saturated (0 = R, 1 = G, 2 = B): bit 21 - c.
    pub const fn color(c: usize) -> u32 {
        1 << (21 - c)
    }
    pub const SZ3_OTZ: u32 = 1 << 18;
    pub const DIVIDE: u32 = 1 << 17;
    pub const MAC0_POS: u32 = 1 << 16;
    pub const MAC0_NEG: u32 = 1 << 15;
    pub const SX2: u32 = 1 << 14;
    pub const SY2: u32 = 1 << 13;
    pub const IR0: u32 = 1 << 12;
}

/// Command word fields (psx-spx "GTE Command Encoding").
pub mod cmd {
    /// Real command number, bits 0..5.
    pub const fn opcode(c: u32) -> u32 {
        c & 0x3F
    }
    /// `lm`: saturate IR1..3 to 0..7FFFh instead of -8000h..7FFFh.
    pub const fn lm(c: u32) -> bool {
        c & (1 << 10) != 0
    }
    /// MVMVA translation vector: 0 TR, 1 BK, 2 FC (bugged), 3 none.
    pub const fn cv(c: u32) -> usize {
        ((c >> 13) & 3) as usize
    }
    /// MVMVA vector: 0 V0, 1 V1, 2 V2, 3 IR1..3.
    pub const fn v(c: u32) -> usize {
        ((c >> 15) & 3) as usize
    }
    /// MVMVA matrix: 0 rotation, 1 light, 2 colour, 3 garbage.
    pub const fn mx(c: u32) -> usize {
        ((c >> 17) & 3) as usize
    }
    /// `sf`: shift results right by 12.
    pub const fn sf(c: u32) -> bool {
        c & (1 << 19) != 0
    }
    /// Encode a command word from its fields (fake number in bits 20..24 is
    /// left zero; the hardware ignores it).
    pub const fn encode(opcode: u32, sf: bool, mx: u32, v: u32, cv: u32, lm: bool) -> u32 {
        (opcode & 0x3F) | (lm as u32) << 10 | (cv & 3) << 13 | (v & 3) << 15 | (mx & 3) << 17 | (sf as u32) << 19
    }
}

/// A documented command: real opcode, mnemonic, cycles (psx-spx table).
pub const COMMANDS: [(u32, &str, u32); 22] = [
    (0x01, "RTPS", 15),
    (0x06, "NCLIP", 8),
    (0x0C, "OP", 6),
    (0x10, "DPCS", 8),
    (0x11, "INTPL", 8),
    (0x12, "MVMVA", 8),
    (0x13, "NCDS", 19),
    (0x14, "CDP", 13),
    (0x16, "NCDT", 44),
    (0x1B, "NCCS", 17),
    (0x1C, "CC", 11),
    (0x1E, "NCS", 14),
    (0x20, "NCT", 30),
    (0x28, "SQR", 5),
    (0x29, "DCPL", 8),
    (0x2A, "DPCT", 17),
    (0x2D, "AVSZ3", 5),
    (0x2E, "AVSZ4", 6),
    (0x30, "RTPT", 23),
    (0x3D, "GPF", 5),
    (0x3E, "GPL", 5),
    (0x3F, "NCCT", 39),
];

/// Mnemonic of a command word's real opcode, if documented.
pub fn command_name(c: u32) -> Option<&'static str> {
    COMMANDS.iter().find(|e| e.0 == cmd::opcode(c)).map(|e| e.1)
}

/// Cycles a command word takes (psx-spx), if documented.
pub fn command_cycles(c: u32) -> Option<u32> {
    COMMANDS.iter().find(|e| e.0 == cmd::opcode(c)).map(|e| e.2)
}

/// The UNR reciprocal table, `unr_table[i] = max(0, (40000h/(i+100h)+1)/2 - 101h)`
/// for i = 0..=100h (psx-spx "GTE Division Inaccuracy").
pub const UNR_TABLE: [u8; 257] = {
    let mut t = [0u8; 257];
    let mut i = 0;
    while i < 257 {
        let v = (0x40000 / (i as i32 + 0x100) + 1) / 2 - 0x101;
        t[i] = if v < 0 { 0 } else { v as u8 };
        i += 1;
    }
    t
};

/// RTPS/RTPT's division `(((H*20000h/SZ3)+1)/2)` as the hardware does it:
/// the UNR approximation, saturated to 1FFFFh. Returns the quotient and
/// whether the divide-overflow flag is set (H >= SZ3*2).
pub fn unr_divide(h: u16, sz3: u16) -> (u32, bool) {
    let (h, sz3) = (h as u32, sz3 as u32);
    if h >= sz3 * 2 {
        return (0x1FFFF, true);
    }
    let z = (sz3 as u16).leading_zeros();
    let n = (h << z) as u64;
    let d = (sz3 << z) as u64;
    let u = UNR_TABLE[((d - 0x7FC0) >> 7) as usize] as u64 + 0x101;
    let d = (0x0200_0080 - d * u) >> 8;
    let d = (0x0000_0080 + d * u) >> 8;
    (((n * d + 0x8000) >> 16).min(0x1FFFF) as u32, false)
}

/// Leading-bit count of LZCS: leading zeroes if positive, ones if negative.
pub fn lzcr(lzcs: u32) -> u32 {
    if lzcs & 0x8000_0000 != 0 { (!lzcs).leading_zeros() } else { lzcs.leading_zeros() }
}

/// ORGB (and IRGB, which reads the same): IR1..3 / 80h, each saturated to
/// 0..1Fh, as 5:5:5. No flags.
pub fn orgb(ir1: i16, ir2: i16, ir3: i16) -> u32 {
    let c = |ir: i16| ((ir as i32) >> 7).clamp(0, 0x1F) as u32;
    c(ir1) | c(ir2) << 5 | c(ir3) << 10
}

const fn sx16(v: u32) -> u32 {
    v as u16 as i16 as i32 as u32
}

const fn lo(w: u32) -> i32 {
    w as u16 as i16 as i32
}

const fn hi(w: u32) -> i32 {
    (w >> 16) as u16 as i16 as i32
}

/// Pack two 16-bit values (SXY, VXY layout: low half first).
const fn pack(lo: i32, hi: i32) -> u32 {
    (lo as u32 & 0xFFFF) | (hi as u32) << 16
}

impl Gte {
    /// A GTE with every register zero.
    pub const fn new() -> Self {
        Gte { data: [0; 32], ctrl: [0; 32] }
    }

    /// The register file at the start of a 64-word region (data, then
    /// control), e.g. the runtime-private page of the guest window.
    pub fn view_mut(words: &mut [u32; 64]) -> &mut Gte {
        // SAFETY: Gte is repr(C) with two [u32; 32] fields: 256 bytes,
        // 4-byte aligned, no padding, every bit pattern valid. The borrow of
        // `words` is carried by the returned reference.
        unsafe { &mut *(words as *mut [u32; 64] as *mut Gte) }
    }

    /// Load every register the way a program would: control registers 0..31
    /// with `ctc2`, then data registers with `mtc2` except SXYP (15, a FIFO
    /// push), IRGB (28, overwrites IR1..3), ORGB (29) and LZCR (31). This
    /// is how the device oracle's runner loads a GTE_OP vector.
    pub fn load(data: &[u32; 32], ctrl: &[u32; 32]) -> Self {
        let mut g = Gte::new();
        for (r, v) in ctrl.iter().enumerate() {
            g.ctc2(r as u32, *v);
        }
        for (r, v) in data.iter().enumerate() {
            if !matches!(r, dr::SXYP | dr::IRGB | dr::ORGB | dr::LZCR) {
                g.mtc2(r as u32, *v);
            }
        }
        g
    }

    /// Every register as `mfc2`/`cfc2` read it.
    pub fn read_all(&self) -> ([u32; 32], [u32; 32]) {
        (core::array::from_fn(|r| self.mfc2(r as u32)), core::array::from_fn(|r| self.cfc2(r as u32)))
    }

    /// Read data register `r` (MFC2/SWC2).
    pub fn mfc2(&self, r: u32) -> u32 {
        let r = (r & 31) as usize;
        match r {
            dr::SXYP => self.data[dr::SXY2],
            dr::IRGB | dr::ORGB => orgb(self.ir(1) as i16, self.ir(2) as i16, self.ir(3) as i16),
            dr::LZCR => lzcr(self.data[dr::LZCS]),
            dr::VZ0 | dr::VZ1 | dr::VZ2 | dr::IR0 | dr::IR1 | dr::IR2 | dr::IR3 => sx16(self.data[r]),
            dr::OTZ | dr::SZ0 | dr::SZ1 | dr::SZ2 | dr::SZ3 => self.data[r] & 0xFFFF,
            _ => self.data[r],
        }
    }

    /// Write data register `r` (MTC2/LWC2). Writes are not saturated and set
    /// no flags: 16-bit registers keep the low half.
    pub fn mtc2(&mut self, r: u32, v: u32) {
        let r = (r & 31) as usize;
        match r {
            dr::VZ0 | dr::VZ1 | dr::VZ2 | dr::IR0 | dr::IR1 | dr::IR2 | dr::IR3 => self.data[r] = sx16(v),
            dr::OTZ | dr::SZ0 | dr::SZ1 | dr::SZ2 | dr::SZ3 => self.data[r] = v & 0xFFFF,
            dr::SXY2 => self.data[r] = v,
            dr::SXYP => {
                self.data[dr::SXY0] = self.data[dr::SXY1];
                self.data[dr::SXY1] = self.data[dr::SXY2];
                self.data[dr::SXY2] = v;
            }
            dr::IRGB => {
                self.data[dr::IR1] = (v & 0x1F) << 7;
                self.data[dr::IR2] = ((v >> 5) & 0x1F) << 7;
                self.data[dr::IR3] = ((v >> 10) & 0x1F) << 7;
            }
            dr::ORGB | dr::LZCR => return,
            _ => self.data[r] = v,
        }
        self.settle();
    }

    /// Read control register `r` (CFC2). RT33, L33, LB3, H, DQA, ZSF3 and
    /// ZSF4 read sign-extended (for H that is the hardware's bug: it is
    /// used unsigned).
    pub fn cfc2(&self, r: u32) -> u32 {
        let r = (r & 31) as usize;
        match r {
            4 | 12 | 20 | cr::H | cr::DQA | cr::ZSF3 | cr::ZSF4 => sx16(self.ctrl[r]),
            cr::FLAG => flag_word(self.ctrl[r]),
            _ => self.ctrl[r],
        }
    }

    /// Write control register `r` (CTC2). FLAG keeps bits 30..12 and
    /// recomputes bit 31.
    pub fn ctc2(&mut self, r: u32, v: u32) {
        let r = (r & 31) as usize;
        self.ctrl[r] = match r {
            4 | 12 | 20 | cr::H | cr::DQA | cr::ZSF3 | cr::ZSF4 => sx16(v),
            cr::FLAG => flag_word(v),
            _ => v,
        };
    }

    /// Keep the derived data registers current for raw readers.
    fn settle(&mut self) {
        self.data[dr::SXYP] = self.data[dr::SXY2];
        let o = orgb(self.ir(1) as i16, self.ir(2) as i16, self.ir(3) as i16);
        self.data[dr::IRGB] = o;
        self.data[dr::ORGB] = o;
        self.data[dr::LZCR] = lzcr(self.data[dr::LZCS]);
    }

    /// FLAG as last computed.
    pub fn flag(&self) -> u32 {
        self.cfc2(cr::FLAG as u32)
    }

    fn ir(&self, n: usize) -> i32 {
        lo(self.data[dr::IR0 + n])
    }

    /// Execute a command (the imm25 of `cop2 imm25`; bits above 24 are
    /// ignored). FLAG is cleared first, as on every command. Of the opcodes
    /// psx-spx lists as N/A, 00h runs as RTPS and 1Ah as DCPL (oracle:
    /// psx-spx only says 00h "modifies similar registers than RTPS"); the
    /// others do nothing else.
    pub fn exec(&mut self, c: u32) {
        let mut e = Exec { g: self, flag: 0, sf: if cmd::sf(c) { 12 } else { 0 }, lm: cmd::lm(c) };
        match cmd::opcode(c) {
            0x00 | 0x01 => e.rtp(0, true),
            0x06 => e.nclip(),
            0x0C => e.op(),
            0x10 => e.dpcs(false),
            0x11 => e.intpl(),
            0x12 => e.mvmva(cmd::mx(c), cmd::v(c), cmd::cv(c)),
            0x13 => e.ncd(0),
            0x14 => e.cdp(),
            0x16 => (0..3).for_each(|v| e.ncd(v)),
            0x1B => e.ncc(0),
            0x1C => e.cc(),
            0x1E => e.nc(0),
            0x20 => (0..3).for_each(|v| e.nc(v)),
            0x28 => e.sqr(),
            0x1A | 0x29 => e.dcpl(),
            0x2A => (0..3).for_each(|_| e.dpcs(true)),
            0x2D => e.avsz3(),
            0x2E => e.avsz4(),
            0x30 => (0..3).for_each(|v| e.rtp(v, v == 2)),
            0x3D => e.gpf(),
            0x3E => e.gpl(),
            0x3F => (0..3).for_each(|v| e.ncc(v)),
            _ => {}
        }
        let f = e.flag;
        self.ctrl[cr::FLAG] = flag_word(f);
        self.settle();
    }
}

/// FLAG as stored and read: bits 30..12, plus bit 31 summarising.
const fn flag_word(v: u32) -> u32 {
    let v = v & flag::WRITABLE;
    if v & flag::ERROR_SOURCES != 0 { v | flag::ERROR } else { v }
}

/// Which matrix a matrix-vector product uses.
#[derive(Clone, Copy)]
enum Mat {
    Rotation,
    Light,
    Color,
    /// MVMVA mx=3: rows [-R*10h, +R*10h, IR0], [RT13 x3], [RT22 x3].
    Garbage,
}

/// Which vector is added (×1000h) before the products.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Add {
    Tr,
    Bk,
    /// MVMVA cv=2: the hardware drops the first partial sum.
    FarColor,
    None,
}

/// One command's execution: the registers, FLAG being accumulated, and the
/// decoded `sf` (as a shift) and `lm`.
struct Exec<'a> {
    g: &'a mut Gte,
    flag: u32,
    sf: u32,
    lm: bool,
}

impl Exec<'_> {
    // ---- operand access -------------------------------------------------

    fn d(&self, r: usize) -> u32 {
        self.g.data[r]
    }

    fn c(&self, r: usize) -> u32 {
        self.g.ctrl[r]
    }

    fn ir(&self, n: usize) -> i32 {
        self.g.ir(n)
    }

    fn irs(&self) -> [i32; 3] {
        [self.ir(1), self.ir(2), self.ir(3)]
    }

    fn ir0(&self) -> i64 {
        self.ir(0) as i64
    }

    fn mac(&self, n: usize) -> i32 {
        self.d(dr::MAC0 + n) as i32
    }

    /// V0, V1 or V2.
    fn vector(&self, n: usize) -> [i32; 3] {
        let xy = self.d(dr::VXY0 + 2 * n);
        [lo(xy), hi(xy), lo(self.d(dr::VZ0 + 2 * n))]
    }

    /// Element (row, col) of a matrix stored as nine halfwords from `base`.
    fn element(&self, base: usize, row: usize, col: usize) -> i32 {
        let k = row * 3 + col;
        let w = self.c(base + k / 2);
        if k.is_multiple_of(2) { lo(w) } else { hi(w) }
    }

    fn matrix(&self, m: Mat) -> [[i32; 3]; 3] {
        let base = match m {
            Mat::Rotation => cr::RT,
            Mat::Light => cr::LLM,
            Mat::Color => cr::LCM,
            Mat::Garbage => {
                let r = ((self.d(dr::RGBC) & 0xFF) << 4) as i32;
                let rt13 = self.element(cr::RT, 0, 2);
                let rt22 = self.element(cr::RT, 1, 1);
                return [[-r, r, self.ir(0)], [rt13; 3], [rt22; 3]];
            }
        };
        core::array::from_fn(|row| core::array::from_fn(|col| self.element(base, row, col)))
    }

    fn add_vector(&self, a: Add) -> [i64; 3] {
        let base = match a {
            Add::Tr => cr::TRX,
            Add::Bk => cr::RBK,
            Add::FarColor => cr::RFC,
            Add::None => return [0; 3],
        };
        core::array::from_fn(|i| self.c(base + i) as i32 as i64)
    }

    fn far_color(&self) -> [i64; 3] {
        self.add_vector(Add::FarColor)
    }

    /// R, G, B of RGBC.
    fn rgbc(&self) -> [i64; 3] {
        let w = self.d(dr::RGBC);
        [(w & 0xFF) as i64, ((w >> 8) & 0xFF) as i64, ((w >> 16) & 0xFF) as i64]
    }

    // ---- saturation and flags --------------------------------------------

    /// One accumulation step into MACn (n = 1..3): flag a result "larger
    /// than 43 bits" (>= 2^43 or < -2^43) and wrap it to 44 bits, as the
    /// hardware's adder does. (The device target's GPL flags 2^43-1 too; a
    /// disputed point, see plans/completed/M04-pixels.md Findings.)
    fn acc(&mut self, n: usize, v: i64) -> i64 {
        if v >= 1 << 43 {
            self.flag |= flag::mac_pos(n);
        } else if v < -(1 << 43) {
            self.flag |= flag::mac_neg(n);
        }
        (v << 20) >> 20
    }

    /// Store MACn (n = 1..3) from a 44-bit accumulator, shifted by sf.
    fn set_mac(&mut self, n: usize, v: i64) {
        self.g.data[dr::MAC0 + n] = (v >> self.sf) as i32 as u32;
    }

    /// IRn's saturation (n = 1..3): -8000h..7FFFh, or 0..7FFFh with `lm`.
    fn sat_ir(&mut self, n: usize, v: i32, lm: bool) -> i32 {
        let min = if lm { 0 } else { -0x8000 };
        if v < min || v > 0x7FFF {
            self.flag |= flag::ir(n);
        }
        v.clamp(min, 0x7FFF)
    }

    fn set_ir(&mut self, n: usize, v: i32) {
        self.g.data[dr::IR0 + n] = v as u32;
    }

    /// [IR1,IR2,IR3] = [MAC1,MAC2,MAC3], saturated per `lm`.
    fn mac_to_ir(&mut self) {
        for n in 1..=3 {
            let v = self.sat_ir(n, self.mac(n), self.lm);
            self.set_ir(n, v);
        }
    }

    /// MAC0's 32-bit overflow flags; returns the value MAC0 stores.
    fn check_mac0(&mut self, v: i64) -> i32 {
        if v > i32::MAX as i64 {
            self.flag |= flag::MAC0_POS;
        } else if v < i32::MIN as i64 {
            self.flag |= flag::MAC0_NEG;
        }
        v as i32
    }

    fn set_mac0(&mut self, v: i64) -> i32 {
        let m = self.check_mac0(v);
        self.g.data[dr::MAC0] = m as u32;
        m
    }

    /// SZ3/OTZ saturation to 0..FFFFh.
    fn sat_z(&mut self, v: i64) -> u32 {
        if !(0..=0xFFFF).contains(&v) {
            self.flag |= flag::SZ3_OTZ;
        }
        v.clamp(0, 0xFFFF) as u32
    }

    /// Push [MAC1/16, MAC2/16, MAC3/16, CODE] onto the colour FIFO.
    fn push_color(&mut self) {
        let mut w = self.d(dr::RGBC) & 0xFF00_0000;
        for c in 0..3 {
            let v = self.mac(c + 1) >> 4;
            if !(0..=0xFF).contains(&v) {
                self.flag |= flag::color(c);
            }
            w |= (v.clamp(0, 0xFF) as u32) << (8 * c);
        }
        self.g.data[dr::RGB0] = self.d(dr::RGB1);
        self.g.data[dr::RGB1] = self.d(dr::RGB2);
        self.g.data[dr::RGB2] = w;
    }

    // ---- shared steps ----------------------------------------------------

    /// MAC = (A*1000h + M*v) SAR sf; IR = MAC, saturated per `lm`.
    fn mat_vec(&mut self, m: Mat, v: [i32; 3], a: Add) {
        let mx = self.matrix(m);
        let t = self.add_vector(a);
        for i in 0..3 {
            let n = i + 1;
            let p = [mx[i][0] as i64 * v[0] as i64, mx[i][1] as i64 * v[1] as i64, mx[i][2] as i64 * v[2] as i64];
            let mut s = self.acc(n, (t[i] << 12) + p[0]);
            if a == Add::FarColor {
                // The far-colour bug: the first partial sum still raises its
                // flags (IR as if lm=0) but is then dropped.
                let first = (s >> self.sf) as i32;
                self.sat_ir(n, first, false);
                s = 0;
            }
            s = self.acc(n, s + p[1]);
            s = self.acc(n, s + p[2]);
            self.set_mac(n, s);
        }
        self.mac_to_ir();
    }

    /// psx-spx's "MAC+(FC-MAC)*IR0" from `base` (the MAC before it):
    /// IR = ((FC SHL 12) - base) SAR sf, saturated as if lm=0; MAC =
    /// (IR*IR0 + base) SAR sf; then IR per `lm` and the colour FIFO.
    fn interpolate(&mut self, base: [i64; 3]) {
        let fc = self.far_color();
        for (n, (f, b)) in (1..=3).zip(fc.iter().zip(base)) {
            let s = self.acc(n, (f << 12) - b);
            self.set_mac(n, s);
        }
        let ir0 = self.ir0();
        for (n, b) in (1..=3).zip(base) {
            let t = self.sat_ir(n, self.mac(n), false) as i64;
            let s = self.acc(n, b + t * ir0);
            self.set_mac(n, s);
        }
        self.mac_to_ir();
        self.push_color();
    }

    /// [R*IR1, G*IR2, B*IR3] SHL 4, for the colour commands.
    fn color_times_ir(&self) -> [i64; 3] {
        let (rgb, ir) = (self.rgbc(), self.irs());
        core::array::from_fn(|i| (rgb[i] << 4) * ir[i] as i64)
    }

    /// Light matrix × V, then BK + colour matrix × IR.
    fn light(&mut self, v: usize) {
        let vec = self.vector(v);
        self.mat_vec(Mat::Light, vec, Add::None);
        let ir = self.irs();
        self.mat_vec(Mat::Color, ir, Add::Bk);
    }

    /// MAC = [R*IR1, G*IR2, B*IR3] SHL 4 SAR sf; IR; colour FIFO.
    fn color_scale(&mut self) {
        for (n, m) in (1..=3).zip(self.color_times_ir()) {
            self.set_mac(n, m);
        }
        self.mac_to_ir();
        self.push_color();
    }

    // ---- commands --------------------------------------------------------

    /// RTPS on vertex `v` (RTPT is three of these); `dq` computes IR0/MAC0.
    fn rtp(&mut self, v: usize, dq: bool) {
        let vec = self.vector(v);
        let rt = self.matrix(Mat::Rotation);
        let tr = self.add_vector(Add::Tr);
        let mut z = 0;
        for i in 0..3 {
            let n = i + 1;
            let mut s = self.acc(n, (tr[i] << 12) + rt[i][0] as i64 * vec[0] as i64);
            s = self.acc(n, s + rt[i][1] as i64 * vec[1] as i64);
            s = self.acc(n, s + rt[i][2] as i64 * vec[2] as i64);
            self.set_mac(n, s);
            z = s;
        }
        for n in 1..=2 {
            let v = self.sat_ir(n, self.mac(n), self.lm);
            self.set_ir(n, v);
        }
        // IR3: stored per lm, but FLAG.22 checks MAC3 SAR 12 (the 44-bit sum)
        // against -8000h..7FFFh whatever sf and lm are.
        let z12 = z >> 12;
        if !(-0x8000..=0x7FFF).contains(&z12) {
            self.flag |= flag::ir(3);
        }
        let min = if self.lm { 0 } else { -0x8000 };
        self.set_ir(3, self.mac(3).clamp(min, 0x7FFF));

        let sz3 = self.sat_z(z12);
        self.g.data[dr::SZ0] = self.d(dr::SZ1);
        self.g.data[dr::SZ1] = self.d(dr::SZ2);
        self.g.data[dr::SZ2] = self.d(dr::SZ3);
        self.g.data[dr::SZ3] = sz3;

        let (q, overflow) = unr_divide(self.c(cr::H) as u16, sz3 as u16);
        if overflow {
            self.flag |= flag::DIVIDE;
        }
        // MAC0 = q*IR1 + OFX, SX2 = MAC0/10000h; then the same for Y. The
        // screen coordinate comes from the full sum, before MAC0's 32 bits.
        let q = q as i64;
        let x = self.c(cr::OFX) as i32 as i64 + self.ir(1) as i64 * q;
        self.set_mac0(x);
        let y = self.c(cr::OFY) as i32 as i64 + self.ir(2) as i64 * q;
        self.set_mac0(y);
        let (sx, sy) = (x >> 16, y >> 16);
        if !(-0x400..=0x3FF).contains(&sx) {
            self.flag |= flag::SX2;
        }
        if !(-0x400..=0x3FF).contains(&sy) {
            self.flag |= flag::SY2;
        }
        self.g.data[dr::SXY0] = self.d(dr::SXY1);
        self.g.data[dr::SXY1] = self.d(dr::SXY2);
        self.g.data[dr::SXY2] = pack(sx.clamp(-0x400, 0x3FF) as i32, sy.clamp(-0x400, 0x3FF) as i32);

        if dq {
            let p = self.c(cr::DQB) as i32 as i64 + lo(self.c(cr::DQA)) as i64 * q;
            self.set_mac0(p);
            let ir0 = p >> 12;
            if !(0..=0x1000).contains(&ir0) {
                self.flag |= flag::IR0;
            }
            self.set_ir(0, ir0.clamp(0, 0x1000) as i32);
        }
    }

    fn nclip(&mut self) {
        let p = |r: usize| {
            let w = self.d(r);
            (lo(w) as i64, hi(w) as i64)
        };
        let ((x0, y0), (x1, y1), (x2, y2)) = (p(dr::SXY0), p(dr::SXY1), p(dr::SXY2));
        self.set_mac0(x0 * y1 + x1 * y2 + x2 * y0 - x0 * y2 - x1 * y0 - x2 * y1);
    }

    /// Cross product of [IR1,IR2,IR3] and [RT11,RT22,RT33].
    fn op(&mut self) {
        let d = [self.element(cr::RT, 0, 0) as i64, self.element(cr::RT, 1, 1) as i64, self.element(cr::RT, 2, 2) as i64];
        let ir = self.irs().map(|v| v as i64);
        let r = [ir[2] * d[1] - ir[1] * d[2], ir[0] * d[2] - ir[2] * d[0], ir[1] * d[0] - ir[0] * d[1]];
        for (n, v) in (1..=3).zip(r) {
            let s = self.acc(n, v);
            self.set_mac(n, s);
        }
        self.mac_to_ir();
    }

    fn mvmva(&mut self, mx: usize, v: usize, cv: usize) {
        let m = [Mat::Rotation, Mat::Light, Mat::Color, Mat::Garbage][mx];
        let vec = if v == 3 { self.irs() } else { self.vector(v) };
        let a = [Add::Tr, Add::Bk, Add::FarColor, Add::None][cv];
        self.mat_vec(m, vec, a);
    }

    fn sqr(&mut self) {
        for n in 1..=3 {
            let i = self.ir(n) as i64;
            self.set_mac(n, i * i);
        }
        self.mac_to_ir();
    }

    /// DPCS, or one step of DPCT (`fifo`: colour from RGB0, not RGBC).
    fn dpcs(&mut self, fifo: bool) {
        let w = if fifo { self.d(dr::RGB0) } else { self.d(dr::RGBC) };
        let base = [(w & 0xFF) as i64, ((w >> 8) & 0xFF) as i64, ((w >> 16) & 0xFF) as i64].map(|c| c << 16);
        self.interpolate(base);
    }

    fn intpl(&mut self) {
        let base = self.irs().map(|v| (v as i64) << 12);
        self.interpolate(base);
    }

    fn dcpl(&mut self) {
        let base = self.color_times_ir();
        self.interpolate(base);
    }

    /// NCS on vertex `v` (NCT is three).
    fn nc(&mut self, v: usize) {
        self.light(v);
        self.push_color();
    }

    /// NCCS on vertex `v` (NCCT is three).
    fn ncc(&mut self, v: usize) {
        self.light(v);
        self.color_scale();
    }

    /// NCDS on vertex `v` (NCDT is three).
    fn ncd(&mut self, v: usize) {
        self.light(v);
        self.dcpl();
    }

    fn cc(&mut self) {
        let ir = self.irs();
        self.mat_vec(Mat::Color, ir, Add::Bk);
        self.color_scale();
    }

    fn cdp(&mut self) {
        let ir = self.irs();
        self.mat_vec(Mat::Color, ir, Add::Bk);
        self.dcpl();
    }

    /// OTZ = MAC0/1000h, saturated. oracle: OTZ comes from the full
    /// product, not from MAC0's 32 bits (when MAC0 overflows, OTZ saturates
    /// the way the product's sign says).
    fn avsz(&mut self, zsf: usize, sum: i64) {
        let p = lo(self.c(zsf)) as i64 * sum;
        self.set_mac0(p);
        let otz = self.sat_z(p >> 12);
        self.g.data[dr::OTZ] = otz;
    }

    fn avsz3(&mut self) {
        let sum = (dr::SZ1..=dr::SZ3).map(|r| (self.d(r) & 0xFFFF) as i64).sum();
        self.avsz(cr::ZSF3, sum);
    }

    fn avsz4(&mut self) {
        let sum = (dr::SZ0..=dr::SZ3).map(|r| (self.d(r) & 0xFFFF) as i64).sum();
        self.avsz(cr::ZSF4, sum);
    }

    fn gpf(&mut self) {
        let ir0 = self.ir0();
        for n in 1..=3 {
            let s = self.acc(n, self.ir(n) as i64 * ir0);
            self.set_mac(n, s);
        }
        self.mac_to_ir();
        self.push_color();
    }

    fn gpl(&mut self) {
        let ir0 = self.ir0();
        for n in 1..=3 {
            let s = self.acc(n, ((self.mac(n) as i64) << self.sf) + self.ir(n) as i64 * ir0);
            self.set_mac(n, s);
        }
        self.mac_to_ir();
        self.push_color();
    }
}

/// Spec-derived checks (psx-spx examples and formulas). Debugging aids: the
/// oracle for this crate is the device target (`psxw oracle check <game>
/// --device gte`).
#[cfg(test)]
mod tests {
    use super::*;

    const RTPS: u32 = 0x0018_0001;
    const MVMVA: u32 = 0x0040_0012;

    #[test]
    fn unr_table_matches_psx_spx() {
        assert_eq!(&UNR_TABLE[..16], &[0xFF, 0xFD, 0xFB, 0xF9, 0xF7, 0xF5, 0xF3, 0xF1, 0xEF, 0xEE, 0xEC, 0xEA, 0xE8, 0xE6, 0xE4, 0xE3]);
        assert_eq!(&UNR_TABLE[0x70..0x80], &[0x63, 0x62, 0x61, 0x60, 0x5F, 0x5E, 0x5D, 0x5D, 0x5C, 0x5B, 0x5A, 0x59, 0x58, 0x57, 0x56, 0x55]);
        assert_eq!(&UNR_TABLE[0xF0..], &[0x07, 0x07, 0x06, 0x06, 0x05, 0x05, 0x04, 0x04, 0x03, 0x03, 0x02, 0x02, 0x01, 0x01, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn unr_division() {
        // Overflow: H >= SZ3*2, including SZ3 = 0.
        assert_eq!(unr_divide(0, 0), (0x1FFFF, true));
        assert_eq!(unr_divide(200, 100), (0x1FFFF, true));
        assert!(!unr_divide(199, 100).1);
        // The special cases psx-spx names.
        assert_eq!(unr_divide(0, 1), (0, false));
        assert_eq!(unr_divide(1, 1), (0x10000, false));
        assert_eq!(unr_divide(0xFE3F, 0x7F20), (0x1FFFF, false));
        assert_eq!(unr_divide(0xF015, 0x780B), (0x1FFFF, false));
        // Close to the exact quotient everywhere else.
        let mut h = 1u32;
        for sz in (1..=0xFFFFu32).step_by(97) {
            h = (h * 1103 + 12345) % (2 * sz).min(0x10000);
            let (q, o) = unr_divide(h as u16, sz as u16);
            assert!(!o);
            let exact = ((h as u64) << 16) / sz as u64;
            assert!(q as u64 + 2 >= exact.min(0x1FFFF) && q as u64 <= exact + 2, "{h:#x}/{sz:#x}: {q:#x} vs {exact:#x}");
        }
    }

    #[test]
    fn leading_bits() {
        assert_eq!(lzcr(0), 32);
        assert_eq!(lzcr(u32::MAX), 32);
        assert_eq!(lzcr(1), 31);
        assert_eq!(lzcr(0x8000_0000), 1);
        assert_eq!(lzcr(0x4000_0000), 1);
        assert_eq!(lzcr(0xFFFF_0000), 16);
        let mut g = Gte::new();
        g.mtc2(dr::LZCS as u32, 0x0000_F000);
        assert_eq!(g.mfc2(dr::LZCR as u32), 16);
        g.mtc2(dr::LZCR as u32, 5);
        assert_eq!(g.mfc2(dr::LZCR as u32), 16);
    }

    #[test]
    fn register_widths_and_quirks() {
        let mut g = Gte::new();
        // 16-bit registers keep the low half, no saturation, no flags.
        g.mtc2(dr::IR1 as u32, 0x1200_8900);
        assert_eq!(g.mfc2(dr::IR1 as u32), 0xFFFF_8900);
        g.mtc2(dr::VZ0 as u32, 0x0001_7FFF);
        assert_eq!(g.mfc2(dr::VZ0 as u32), 0x7FFF);
        g.mtc2(dr::SZ1 as u32, 0xFFFF_8000);
        assert_eq!(g.mfc2(dr::SZ1 as u32), 0x8000);
        g.mtc2(dr::OTZ as u32, 0x1_2345);
        assert_eq!(g.mfc2(dr::OTZ as u32), 0x2345);
        // H reads sign-extended; RT33, L33, LB3, DQA, ZSF3/4 too.
        g.ctc2(cr::H as u32, 0x0000_8000);
        assert_eq!(g.cfc2(cr::H as u32), 0xFFFF_8000);
        for r in [4u32, 12, 20, 27, 29, 30] {
            g.ctc2(r, 0x1234_F000);
            assert_eq!(g.cfc2(r), 0xFFFF_F000, "cnt{r}");
        }
        g.ctc2(0, 0xF000_8000);
        assert_eq!(g.cfc2(0), 0xF000_8000);
        // FLAG: bits 30..12 writable; bit 31 summarises 30..23 and 18..13.
        g.ctc2(cr::FLAG as u32, 0xFFFF_FFFF);
        assert_eq!(g.cfc2(cr::FLAG as u32), 0xFFFF_F000);
        g.ctc2(cr::FLAG as u32, flag::DIVIDE);
        assert_eq!(g.cfc2(cr::FLAG as u32), flag::DIVIDE | flag::ERROR);
        g.ctc2(cr::FLAG as u32, flag::IR0 | flag::color(0));
        assert_eq!(g.cfc2(cr::FLAG as u32), flag::IR0 | flag::color(0));
    }

    #[test]
    fn fifos() {
        let mut g = Gte::new();
        g.mtc2(dr::SXY0 as u32, 1);
        g.mtc2(dr::SXY1 as u32, 2);
        g.mtc2(dr::SXY2 as u32, 3);
        assert_eq!(g.mfc2(dr::SXYP as u32), 3);
        g.mtc2(dr::SXYP as u32, 4);
        assert_eq!([12u32, 13, 14, 15].map(|r| g.mfc2(r)), [2, 3, 4, 4]);
        // RTPS pushes both FIFOs.
        for (r, v) in [(16u32, 10), (17, 11), (18, 12), (19, 13)] {
            g.mtc2(r, v);
        }
        g.ctc2(cr::TRX as u32 + 2, 1000);
        g.exec(RTPS);
        assert_eq!([16u32, 17, 18, 19].map(|r| g.mfc2(r)), [11, 12, 13, 1000]);
        assert_eq!([g.mfc2(12), g.mfc2(13)], [3, 4]);
    }

    #[test]
    fn irgb_orgb() {
        let mut g = Gte::new();
        g.mtc2(dr::IRGB as u32, 0xFFFF_7C1F);
        assert_eq!([9u32, 10, 11].map(|r| g.mfc2(r)), [0x1F << 7, 0, 0x1F << 7]);
        assert_eq!(g.mfc2(dr::ORGB as u32), 0x7C1F);
        assert_eq!(g.mfc2(dr::IRGB as u32), 0x7C1F);
        g.mtc2(dr::IR1 as u32, 0xFFFF_FFFF);
        g.mtc2(dr::IR2 as u32, 0x1000);
        g.mtc2(dr::IR3 as u32, 0x0080);
        assert_eq!(g.mfc2(dr::ORGB as u32), 0x1F << 5 | 1 << 10);
        g.mtc2(dr::ORGB as u32, 0);
        assert_eq!(g.mfc2(dr::ORGB as u32), 0x1F << 5 | 1 << 10);
    }

    /// Identity rotation, TR = 0, V0 = (x, y, z), H, OF = 0: RTPS projects.
    #[test]
    fn rtps_projects() {
        let mut g = Gte::new();
        g.ctc2(0, 0x1000);
        g.ctc2(2, 0x1000);
        g.ctc2(4, 0x1000);
        g.ctc2(cr::H as u32, 200);
        g.ctc2(cr::DQA as u32, 1);
        g.mtc2(dr::VXY0 as u32, pack(100, -50));
        g.mtc2(dr::VZ0 as u32, 400);
        g.exec(RTPS);
        assert_eq!(g.mfc2(dr::SZ3 as u32), 400);
        let q = unr_divide(200, 400).0 as i32;
        assert_eq!(q, 0x8000);
        assert_eq!(g.mfc2(dr::SXY2 as u32), pack(50, -25));
        assert_eq!(g.mfc2(dr::MAC0 as u32), q as u32);
        assert_eq!(g.flag(), 0);
        // Behind the camera: SZ3 saturates to 0 and the division overflows.
        g.mtc2(dr::VZ0 as u32, (-5i32) as u32);
        g.exec(RTPS);
        assert_eq!(g.mfc2(dr::SZ3 as u32), 0);
        assert_eq!(g.mfc2(dr::SXY2 as u32), pack(199, -100));
        assert_eq!(g.flag(), flag::ERROR | flag::SZ3_OTZ | flag::DIVIDE);
    }

    #[test]
    fn mvmva_far_color_drops_the_first_column() {
        let mut g = Gte::new();
        for (r, v) in [(0u32, pack(0x1000, 0x0800)), (1, pack(0x0400, 0)), (2, 0), (3, 0), (4, 0)] {
            g.ctc2(r, v);
        }
        g.ctc2(cr::RFC as u32, 0x7FFF_FFFF);
        g.mtc2(dr::VXY0 as u32, pack(10, 20));
        g.mtc2(dr::VZ0 as u32, 30);
        g.exec(MVMVA | cmd::encode(0, true, 0, 0, 2, false));
        // MAC1 = (RT12*VY0 + RT13*VZ0) SAR 12; the FC partial sum only flags.
        assert_eq!(g.mfc2(dr::MAC1 as u32) as i32, (0x0800 * 20 + 0x0400 * 30) >> 12);
        assert_eq!(g.flag() & flag::ir(1), flag::ir(1));
        // cv=3 (none) with the garbage matrix: row 2 is RT13 three times.
        g.mtc2(dr::RGBC as u32, 0x02);
        g.exec(MVMVA | cmd::encode(0, false, 3, 0, 3, false));
        assert_eq!(g.mfc2(dr::MAC1 as u32) as i32, -(2 << 4) * 10 + (2 << 4) * 20);
        assert_eq!(g.mfc2(dr::MAC2 as u32) as i32, 0x0400 * 60);
    }

    #[test]
    fn color_fifo_and_saturation() {
        let mut g = Gte::new();
        g.mtc2(dr::RGBC as u32, 0x3000_0000);
        g.mtc2(dr::IR0 as u32, 0x1000);
        g.mtc2(dr::IR1 as u32, 0x0FF0);
        g.mtc2(dr::IR2 as u32, 0x1000);
        g.mtc2(dr::IR3 as u32, (-16i32) as u32);
        g.mtc2(dr::RGB1 as u32, 0x11);
        g.mtc2(dr::RGB2 as u32, 0x22);
        let gpf = 0x0190_003D | 1 << 19;
        g.exec(gpf);
        assert_eq!([20u32, 21].map(|r| g.mfc2(r)), [0x11, 0x22]);
        assert_eq!(g.mfc2(dr::RGB2 as u32), 0x3000_FFFF);
        assert_eq!(g.flag(), flag::color(1) | flag::color(2));
        // lm=1 clamps IR3 to 0 and flags it; bit 31 ignores IR3 (bit 22).
        g.mtc2(dr::IR3 as u32, (-16i32) as u32);
        g.exec(gpf | 1 << 10);
        assert_eq!(g.mfc2(dr::IR3 as u32), 0);
        assert_eq!(g.flag(), flag::color(1) | flag::color(2) | flag::ir(3));
        g.mtc2(dr::IR1 as u32, 0x8000);
        g.exec(gpf | 1 << 10);
        assert_eq!(g.flag() & (flag::ir(1) | flag::ERROR), flag::ir(1) | flag::ERROR);
    }

    #[test]
    fn mac_overflow_wraps_to_44_bits() {
        let mut g = Gte::new();
        g.ctc2(0, 0x7FFF);
        g.ctc2(cr::TRX as u32, 0x7FFF_FFFF);
        g.mtc2(dr::VXY0 as u32, 0x7FFF);
        g.exec(MVMVA | cmd::encode(0, true, 0, 0, 0, false));
        assert_eq!(g.flag() & flag::mac_pos(1), flag::mac_pos(1));
        let wrapped = (((0x7FFF_FFFFi64 << 12) + 0x7FFF * 0x7FFF) << 20) >> 20;
        assert_eq!(g.mfc2(dr::MAC1 as u32) as i32, (wrapped >> 12) as i32);
    }

    #[test]
    fn view_and_load() {
        let mut words = [0u32; 64];
        Gte::view_mut(&mut words).mtc2(dr::IR2 as u32, 0x8000);
        assert_eq!(words[dr::IR2], 0xFFFF_8000);
        let g = Gte::load(&[7; 32], &[0xFFFF_FFFF; 32]);
        let (d, c) = g.read_all();
        assert_eq!(d[dr::SXYP], 7);
        assert_eq!(d[dr::LZCR], 29);
        assert_eq!(c[cr::FLAG], 0xFFFF_F000);
        assert_eq!(c[cr::H], 0xFFFF_FFFF);
        assert_eq!(command_name(0x0028_0030), Some("RTPT"));
        assert_eq!(command_cycles(0x00F8_0416), Some(44));
    }
}
