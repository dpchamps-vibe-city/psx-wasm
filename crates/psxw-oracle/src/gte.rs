//! The GTE in the oracle (plans/completed/M04-pixels.md tasks 1–2):
//!
//! - [`Device`]: psx-gte as a [`DeviceUnderTest`]. It runs a GTE_OP vector
//!   exactly as the runner does (`oracle/runner/gte.c`): control registers
//!   0..31 with `ctc2`, data registers with `mtc2` except
//!   [`wire::GTE_DATA_SKIP_MASK`], the command, then all 64 read back.
//! - [`GteHook`]: the recording hook the runtime calls before a command, to
//!   record each distinct (command, input registers) as a vector.
//! - Synthetic generators ([`GENERATORS`]): seeded, reproducible GTE_OP
//!   vectors covering every command with its `sf`/`lm` (and MVMVA's
//!   `mx`/`v`/`cv`) combinations, saturation boundaries, the division and
//!   its overflow, the FIFOs, LZCS→LZCR and IRGB/ORGB. They go to the sets
//!   `synthetic-gte-<generator>`, which `check` includes.
//! - [`disputed`]: inputs where the device target and psx-spx disagree,
//!   kept in [`DISPUTED_SET`] out of the verdicts until hardware settles
//!   them.
//!
//! The generators only choose inputs (the command chains use psx-gte to
//! advance a state); the goldens always come from the device target.

use crate::commands::DeviceUnderTest;
use crate::exact::Rng;
use crate::record::Recorder;
use crate::store::Store;
use crate::vector::{GteOp, Vector, VectorId};
use crate::wire;
use anyhow::{Result, bail};
use psx_gte::{Gte, cmd, cr, dr};
use std::collections::BTreeMap;

/// Default generator seed ("PSXW", "M4", 1).
pub const DEFAULT_SEED: u64 = 0x5053_5857_4D34_0001;

/// The synthetic generators, in order.
pub const GENERATORS: [&str; 7] = ["commands", "saturation", "division", "fifo", "lzcr", "orgb", "registers"];

/// The set a generator's vectors go to (`check` includes every set named
/// with [`crate::commands::SYNTHETIC_PREFIX`]).
pub fn set_name(generator: &str) -> String {
    format!("{}gte-{generator}", crate::commands::SYNTHETIC_PREFIX)
}

// ---- device under test -----------------------------------------------------

/// psx-gte, run the way the runner runs a GTE_OP vector.
pub fn run_op(op: &GteOp) -> Vec<u8> {
    let mut g = Gte::new();
    for (r, v) in op.ctrl.iter().enumerate() {
        g.ctc2(r as u32, *v);
    }
    for (r, v) in op.data.iter().enumerate() {
        if wire::GTE_DATA_SKIP_MASK & (1 << r) == 0 {
            g.mtc2(r as u32, *v);
        }
    }
    g.exec(op.cmd & 0x01FF_FFFF);
    let (d, c) = g.read_all();
    d.iter().chain(&c).flat_map(|w| w.to_le_bytes()).collect()
}

/// psx-gte as the oracle's `gte` device.
#[derive(Default)]
pub struct Device;

impl DeviceUnderTest for Device {
    fn device(&self) -> &str {
        "gte"
    }

    fn name(&self) -> String {
        "psx-gte".into()
    }

    fn run(&mut self, v: &Vector, _: &Store) -> Result<Vec<u8>> {
        match v {
            Vector::GteOp(op) => Ok(run_op(op)),
            other => bail!("the gte device cannot run a {} vector", other.kind()),
        }
    }
}

// ---- recording hook ----------------------------------------------------------

/// The GTE_OP vector for executing `cmd` on a GTE whose registers read as
/// `data`/`ctrl`, in canonical form: registers as the hardware reads them
/// back (so writes that differ only in ignored bits record once), derived
/// registers (SXYP, IRGB, ORGB, LZCR) as they read, FLAG zero (every
/// command clears it), and the command as its imm25.
pub fn canonical_op(data: &[u32; 32], ctrl: &[u32; 32], cmd: u32) -> GteOp {
    let mut g = Gte::new();
    for (r, v) in ctrl.iter().enumerate() {
        g.ctc2(r as u32, *v);
    }
    for (r, v) in data.iter().enumerate() {
        if wire::GTE_DATA_SKIP_MASK & (1 << r) == 0 {
            g.mtc2(r as u32, *v);
        }
    }
    let (data, mut ctrl) = g.read_all();
    ctrl[cr::FLAG] = 0;
    GteOp { data, ctrl, cmd: cmd & 0x01FF_FFFF }
}

/// The recording hook: the runtime calls [`GteHook::before_exec`] with its
/// GTE state just before executing a command, and each distinct (command,
/// input registers) becomes a GTE_OP vector in the recorder (normally a
/// `StoreRecorder` on the set `game-<id>`). At most `per_command` vectors
/// are recorded per command word, so a long run spreads its budget over
/// every command the game uses instead of the first hot loop.
pub struct GteHook {
    per_command: usize,
    counts: BTreeMap<u32, usize>,
}

impl GteHook {
    pub fn new(per_command: usize) -> Self {
        GteHook { per_command, counts: BTreeMap::new() }
    }

    /// Record `cmd` about to run on `gte`. Returns the new vector's id, or
    /// None if it was already recorded or the command's budget is spent.
    pub fn before_exec(&mut self, rec: &mut dyn Recorder, gte: &Gte, cmd: u32) -> Result<Option<VectorId>> {
        let (data, ctrl) = gte.read_all();
        self.before_exec_raw(rec, &data, &ctrl, cmd)
    }

    /// The same, from the 64 register words (data, then control) as the
    /// runtime keeps them.
    pub fn before_exec_raw(&mut self, rec: &mut dyn Recorder, data: &[u32; 32], ctrl: &[u32; 32], cmd: u32) -> Result<Option<VectorId>> {
        let op = canonical_op(data, ctrl, cmd);
        let n = self.counts.entry(op.cmd).or_default();
        if *n >= self.per_command {
            return Ok(None);
        }
        let id = rec.record(Vector::GteOp(op))?;
        if id.is_some() {
            *n += 1;
        }
        Ok(id)
    }

    /// Vectors recorded so far, by command word.
    pub fn counts(&self) -> &BTreeMap<u32, usize> {
        &self.counts
    }
}

// ---- generators ----------------------------------------------------------------

/// A generated vector and the generator that made it.
pub struct Generated {
    pub generator: &'static str,
    pub vector: Vector,
}

/// Every synthetic vector for `seed`, generator by generator.
pub fn generate(seed: u64) -> Vec<Generated> {
    let mut out = Vec::new();
    for (k, name) in GENERATORS.iter().enumerate() {
        let mut g = Gen { rng: Rng::new(seed ^ (k as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15)), out: Vec::new() };
        match *name {
            "commands" => g.commands(),
            "saturation" => g.saturation(),
            "division" => g.division(),
            "fifo" => g.fifo(),
            "lzcr" => g.lzcr(),
            "orgb" => g.orgb(),
            _ => g.registers(),
        }
        out.extend(g.out.into_iter().map(|vector| Generated { generator: name, vector }));
    }
    out
}

/// Store the synthetic vectors and add them to their sets, and the
/// disputed vectors to [`DISPUTED_SET`]. Returns how many each generator
/// made.
pub fn install(store: &Store, seed: u64) -> Result<BTreeMap<&'static str, usize>> {
    let mut sets: BTreeMap<&'static str, Vec<VectorId>> = BTreeMap::new();
    for g in generate(seed) {
        let id = store.put_vector(&g.vector)?;
        sets.entry(g.generator).or_default().push(id);
    }
    let mut counts = BTreeMap::new();
    for (name, ids) in sets {
        counts.insert(name, ids.len());
        store.replace_set(&set_name(name), ids)?;
    }
    let disputed = disputed().iter().map(|v| store.put_vector(v)).collect::<Result<Vec<_>>>()?;
    store.replace_set(DISPUTED_SET, disputed)?;
    Ok(counts)
}

/// Where the device target and psx-spx disagree, so that only hardware
/// (M10) can settle it. These vectors are captured like any other but kept
/// out of `check`'s verdicts; `check` reports how psx-gte fares on them in a
/// note. Each case is a Finding in plans/completed/M04-pixels.md.
pub const DISPUTED_SET: &str = "disputed-gte";

/// The disputed vectors (fixed, not seeded).
///
/// GPL with sf=1 where MAC SHL 12 + IR*IR0 is exactly 2^43-1: psx-spx
/// flags MAC overflow only for results "larger than 43 bits" (psx-gte
/// does not flag); Beetle's GPL sets the positive-overflow flag at 2^43-1
/// already (its `i64_to_i44` compares `>= 0x7FFFFFFFFFF`, where its MVMVA
/// path compares `>= 1 << 43`).
pub fn disputed() -> Vec<Vector> {
    let mut g = Gen { rng: Rng::new(DEFAULT_SEED ^ 0xD15B_0001), out: Vec::new() };
    for lm in [false, true] {
        for (ir0, ir) in [(1, 0xFFF), (3, 0x555), (-1, -0xFFF)] {
            let mut st = g.plausible();
            for n in 0..3 {
                st.d[dr::MAC1 + n] = 0x7FFF_FFFF;
            }
            st.d[dr::IR0] = ir0 as u32;
            st.ir([ir, ir, ir]);
            g.push(&st, op("GPL", true, lm));
        }
    }
    g.out
}

/// Real opcode, mnemonic and the fake command number Sony's SDK puts in
/// bits 20..24 (psx-spx's table; the hardware ignores it).
const OPS: [(u32, &str, u32); 22] = [
    (0x01, "RTPS", 0x01),
    (0x06, "NCLIP", 0x14),
    (0x0C, "OP", 0x17),
    (0x10, "DPCS", 0x07),
    (0x11, "INTPL", 0x09),
    (0x12, "MVMVA", 0x04),
    (0x13, "NCDS", 0x0E),
    (0x14, "CDP", 0x12),
    (0x16, "NCDT", 0x0F),
    (0x1B, "NCCS", 0x10),
    (0x1C, "CC", 0x13),
    (0x1E, "NCS", 0x0C),
    (0x20, "NCT", 0x0D),
    (0x28, "SQR", 0x0A),
    (0x29, "DCPL", 0x06),
    (0x2A, "DPCT", 0x0F),
    (0x2D, "AVSZ3", 0x15),
    (0x2E, "AVSZ4", 0x16),
    (0x30, "RTPT", 0x02),
    (0x3D, "GPF", 0x19),
    (0x3E, "GPL", 0x1A),
    (0x3F, "NCCT", 0x11),
];

/// Opcodes psx-spx lists as N/A that the device target runs as another
/// command (00h as RTPS, 1Ah as DCPL).
const ALIASES: [u32; 2] = [0x00, 0x1A];

/// A command word by mnemonic, with the SDK's fake number.
fn op(name: &str, sf: bool, lm: bool) -> u32 {
    let (o, _, fake) = OPS.iter().find(|e| e.1 == name).copied().unwrap_or((0x01, "RTPS", 0x01));
    fake << 20 | cmd::encode(o, sf, 0, 0, 0, lm)
}

fn mvmva(sf: bool, mx: u32, v: u32, cv: u32, lm: bool) -> u32 {
    0x04 << 20 | cmd::encode(0x12, sf, mx, v, cv, lm)
}

/// Values at the edges of a signed 16-bit field and of the fixed-point
/// formats the GTE uses (1.0 = 1000h).
const EDGE16: [i32; 19] =
    [0, 1, -1, 2, -2, 0x7FFF, -0x8000, 0x7FFE, -0x7FFF, 0x1000, -0x1000, 0x0FFF, 0x4000, -0x4000, 0x00FF, 0x0100, 0x0080, 0x07FF, 0x0800];

/// Edges of 32-bit fields.
const EDGE32: [i64; 16] = [
    0,
    1,
    -1,
    0x7FFF_FFFF,
    -0x8000_0000,
    0x7FFF,
    -0x8000,
    0xFFFF,
    0x1_0000,
    0x1000,
    -0x1000,
    0x00FF_FFFF,
    0x0080_0000,
    0x7FFF_F000,
    0x4000_0000,
    -0x4000_0000,
];

/// How a random state picks its values.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fl {
    /// Ranges a game would use.
    Plausible,
    /// Mostly edge values.
    Edge,
    /// Every register a random 32-bit word (garbage in ignored bits too).
    Wide,
    /// Each register picks one of the above.
    Mixed,
}

const FLAVOURS: [Fl; 4] = [Fl::Plausible, Fl::Edge, Fl::Wide, Fl::Mixed];

/// A register's plausible contents.
#[derive(Clone, Copy)]
enum Spec {
    /// Two signed halves in lo..=hi.
    Pair(i32, i32),
    /// One signed 16-bit value in lo..=hi (upper half sign extension).
    S16(i32, i32),
    /// A 32-bit value in lo..=hi.
    W32(i64, i64),
    /// Any word.
    Any,
}

const DATA_SPEC: [Spec; 32] = [
    Spec::Pair(-0x800, 0x800),  // VXY0
    Spec::S16(-0x800, 0x800),   // VZ0
    Spec::Pair(-0x800, 0x800),  // VXY1
    Spec::S16(-0x800, 0x800),   // VZ1
    Spec::Pair(-0x800, 0x800),  // VXY2
    Spec::S16(-0x800, 0x800),   // VZ2
    Spec::Any,                  // RGBC
    Spec::W32(0, 0xFFFF),       // OTZ
    Spec::S16(0, 0x1000),       // IR0
    Spec::S16(-0x1000, 0x1000), // IR1
    Spec::S16(-0x1000, 0x1000), // IR2
    Spec::S16(-0x1000, 0x1000), // IR3
    Spec::Pair(-0x400, 0x3FF),  // SXY0
    Spec::Pair(-0x400, 0x3FF),  // SXY1
    Spec::Pair(-0x400, 0x3FF),  // SXY2
    Spec::Any,                  // SXYP (not written)
    Spec::W32(0, 0xFFFF),       // SZ0
    Spec::W32(0, 0xFFFF),       // SZ1
    Spec::W32(0, 0xFFFF),       // SZ2
    Spec::W32(0, 0xFFFF),       // SZ3
    Spec::Any,                  // RGB0
    Spec::Any,                  // RGB1
    Spec::Any,                  // RGB2
    Spec::Any,                  // RES1
    Spec::W32(-0x10_0000, 0x10_0000), // MAC0
    Spec::W32(-0x1_0000, 0x1_0000),   // MAC1
    Spec::W32(-0x1_0000, 0x1_0000),   // MAC2
    Spec::W32(-0x1_0000, 0x1_0000),   // MAC3
    Spec::Any,                        // IRGB (not written)
    Spec::Any,                        // ORGB (read-only)
    Spec::Any,                        // LZCS
    Spec::Any,                        // LZCR (read-only)
];

const CTRL_SPEC: [Spec; 32] = [
    Spec::Pair(-0x1000, 0x1000), // RT11 RT12
    Spec::Pair(-0x1000, 0x1000), // RT13 RT21
    Spec::Pair(-0x1000, 0x1000), // RT22 RT23
    Spec::Pair(-0x1000, 0x1000), // RT31 RT32
    Spec::S16(-0x1000, 0x1000),  // RT33
    Spec::W32(-0x1000, 0x1000),  // TRX
    Spec::W32(-0x1000, 0x1000),  // TRY
    Spec::W32(0x200, 0x4000),    // TRZ
    Spec::Pair(-0x1000, 0x1000), // L11 L12
    Spec::Pair(-0x1000, 0x1000), // L13 L21
    Spec::Pair(-0x1000, 0x1000), // L22 L23
    Spec::Pair(-0x1000, 0x1000), // L31 L32
    Spec::S16(-0x1000, 0x1000),  // L33
    Spec::W32(0, 0x1000),        // RBK
    Spec::W32(0, 0x1000),        // GBK
    Spec::W32(0, 0x1000),        // BBK
    Spec::Pair(0, 0x1000),       // LR1 LR2
    Spec::Pair(0, 0x1000),       // LR3 LG1
    Spec::Pair(0, 0x1000),       // LG2 LG3
    Spec::Pair(0, 0x1000),       // LB1 LB2
    Spec::S16(0, 0x1000),        // LB3
    Spec::W32(0, 0xFF0),         // RFC
    Spec::W32(0, 0xFF0),         // GFC
    Spec::W32(0, 0xFF0),         // BFC
    Spec::W32(0, 640 << 16),     // OFX
    Spec::W32(0, 480 << 16),     // OFY
    Spec::W32(0x40, 0x800),      // H
    Spec::S16(-0x200, 0),        // DQA
    Spec::W32(0, 0x0140_0000),   // DQB
    Spec::S16(0, 0x400),         // ZSF3
    Spec::S16(0, 0x300),         // ZSF4
    Spec::Any,                   // FLAG (cleared by every command)
];

fn pair(lo: i32, hi: i32) -> u32 {
    (lo as u32 & 0xFFFF) | (hi as u32) << 16
}

/// A GTE state being built: the data and control registers of a vector.
#[derive(Clone)]
struct St {
    d: [u32; 32],
    c: [u32; 32],
}

impl St {
    fn op(&self, cmd: u32) -> Vector {
        Vector::GteOp(GteOp { data: self.d, ctrl: self.c, cmd })
    }

    /// Set a 3×3 matrix stored as nine halfwords from control word `base`.
    fn mat(&mut self, base: usize, m: [[i32; 3]; 3]) {
        for k in 0..9 {
            let v = m[k / 3][k % 3] as u32 & 0xFFFF;
            let w = &mut self.c[base + k / 2];
            *w = if k % 2 == 0 { (*w & 0xFFFF_0000) | v } else { (*w & 0xFFFF) | v << 16 };
        }
    }

    /// V0, V1 or V2.
    fn vec(&mut self, n: usize, v: [i32; 3]) {
        self.d[2 * n] = pair(v[0], v[1]);
        self.d[2 * n + 1] = v[2] as u32;
    }

    /// Three consecutive control words (TR, BK, FC).
    fn ctl3(&mut self, base: usize, v: [i64; 3]) {
        for (i, x) in v.iter().enumerate() {
            self.c[base + i] = *x as u32;
        }
    }

    fn ir(&mut self, v: [i32; 3]) {
        for (i, x) in v.iter().enumerate() {
            self.d[dr::IR1 + i] = *x as u32;
        }
    }

    /// The state after running `cmd` in psx-gte (for command chains).
    fn after(&self, cmd: u32) -> St {
        let mut g = Gte::load(&self.d, &self.c);
        g.exec(cmd);
        let (d, c) = g.read_all();
        St { d, c }
    }
}

const IDENTITY: [[i32; 3]; 3] = [[0x1000, 0, 0], [0, 0x1000, 0], [0, 0, 0x1000]];
const ZERO: [[i32; 3]; 3] = [[0; 3]; 3];

struct Gen {
    rng: Rng,
    out: Vec<Vector>,
}

impl Gen {
    fn push(&mut self, st: &St, cmd: u32) {
        self.out.push(st.op(cmd));
    }

    fn int(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.rng.next_u64() % (hi - lo + 1) as u64) as i64
    }

    fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.rng.below(xs.len() as u32) as usize]
    }

    fn bool(&mut self) -> bool {
        self.rng.chance(2)
    }

    fn word(&mut self, spec: Spec, fl: Fl) -> u32 {
        let fl = if fl == Fl::Mixed { self.pick(&[Fl::Plausible, Fl::Edge, Fl::Wide]) } else { fl };
        match (fl, spec) {
            (Fl::Wide, _) | (_, Spec::Any) => self.rng.next_u64() as u32,
            (Fl::Edge, Spec::Pair(..)) => pair(self.pick(&EDGE16), self.pick(&EDGE16)),
            (Fl::Edge, Spec::S16(..)) => self.pick(&EDGE16) as u32,
            (Fl::Edge, Spec::W32(..)) if !self.rng.chance(4) => self.pick(&EDGE32) as u32,
            (_, Spec::Pair(lo, hi)) => pair(self.int(lo as i64, hi as i64) as i32, self.int(lo as i64, hi as i64) as i32),
            (_, Spec::S16(lo, hi)) => self.int(lo as i64, hi as i64) as u32,
            (_, Spec::W32(lo, hi)) => self.int(lo, hi) as u32,
        }
    }

    fn state(&mut self, fl: Fl) -> St {
        let mut st = St { d: [0; 32], c: [0; 32] };
        for r in 0..32 {
            st.d[r] = self.word(DATA_SPEC[r], fl);
            st.c[r] = self.word(CTRL_SPEC[r], fl);
        }
        st
    }

    fn plausible(&mut self) -> St {
        self.state(Fl::Plausible)
    }

    /// Bits the hardware ignores (6..9, 11..12, 20..24, and the MVMVA
    /// fields on other commands), randomised.
    fn ignored_bits(&mut self, c: u32, mvmva: bool) -> u32 {
        let mut mask = 0x0F << 6 | 3 << 11 | 0x1F << 20;
        if !mvmva {
            mask |= 0x3F << 13;
        }
        let r = self.rng.next_u64() as u32;
        (c & !mask) | (r & mask)
    }

    // ---- commands: every command word variant on random states ----------------

    fn commands(&mut self) {
        for &(o, name, _) in OPS.iter() {
            if o == 0x12 {
                continue;
            }
            for sf in [false, true] {
                for lm in [false, true] {
                    for k in 0..16 {
                        let st = self.state(FLAVOURS[k % 4]);
                        let mut c = op(name, sf, lm);
                        if k % 4 == 3 {
                            c = self.ignored_bits(c, false);
                        }
                        self.push(&st, c);
                    }
                }
            }
        }
        // 00h and 1Ah, which psx-spx lists as N/A: the oracle runs them as
        // RTPS and DCPL.
        for o in ALIASES {
            for sf in [false, true] {
                for lm in [false, true] {
                    for fl in FLAVOURS {
                        let st = self.state(fl);
                        self.push(&st, cmd::encode(o, sf, 0, 0, 0, lm));
                    }
                }
            }
        }
        for mx in 0..4 {
            for v in 0..4 {
                for cv in 0..4 {
                    for sf in [false, true] {
                        for lm in [false, true] {
                            for k in 0..6 {
                                let st = self.state(FLAVOURS[k % 4]);
                                let mut c = mvmva(sf, mx, v, cv, lm);
                                if k == 5 {
                                    c = self.ignored_bits(c, true);
                                }
                                self.push(&st, c);
                            }
                        }
                    }
                }
            }
        }
    }

    // ---- saturation boundaries --------------------------------------------------

    fn saturation(&mut self) {
        self.sat_ir_mvmva();
        self.sat_mac44();
        self.sat_color();
        self.sat_rtps_z();
        self.sat_rtps_xy();
        self.sat_rtps_dq();
        self.sat_avsz();
        self.sat_nclip();
        self.sat_far_color();
        self.sat_interpolation();
    }

    /// A row sum `TR*1000h + M*V` that is exactly `s`: TR = floor(s/1000h)
    /// and the remainder through the first matrix column with V = (1, ..).
    fn split(s: i64) -> (i64, i32) {
        let t = s.div_euclid(0x1000).clamp(-0x8000_0000, 0x7FFF_FFFF);
        let r = s - t * 0x1000;
        (t, r.clamp(-0x8000, 0x7FFF) as i32)
    }

    /// IR1..3 at their lm=0 and lm=1 limits, through MVMVA (RT·V0 + TR).
    fn sat_ir_mvmva(&mut self) {
        const T: [i64; 14] = [-0x8001, -0x8000, -0x7FFF, -1, 0, 1, 0x7FFE, 0x7FFF, 0x8000, 0x8001, 0x7FFF_FFFF, -0x8000_0000, 0xFFFF, -0x1_0000];
        for sf in [false, true] {
            for lm in [false, true] {
                for j in 0..T.len() {
                    for extra in 0..2 {
                        let mut st = self.plausible();
                        let mut m = ZERO;
                        let mut tr = [0i64; 3];
                        for row in 0..3 {
                            let mac = T[(j + 5 * row) % T.len()];
                            // sf=1: MAC = sum >> 12, any low bits; sf=0: MAC = sum.
                            let low = if sf { self.int(0, 0xFFF) } else { 0 };
                            let sum = if sf { mac * 0x1000 + low } else { mac };
                            let (t, r) = Self::split(sum);
                            tr[row] = t;
                            m[row][0] = r;
                            if extra == 1 {
                                // The same sum spread over all three columns.
                                let a = self.int(-0x100, 0x100) as i32;
                                m[row][0] = r - a;
                                m[row][1] = a;
                            }
                        }
                        st.mat(cr::RT, m);
                        st.ctl3(cr::TRX, tr);
                        st.vec(0, [1, 1, 0]);
                        self.push(&st, mvmva(sf, 0, 0, 0, lm));
                    }
                }
            }
        }
    }

    /// MAC1..3 at ±2^43, including overflows in an intermediate sum that
    /// the later products bring back into range.
    fn sat_mac44(&mut self) {
        const P: i64 = 0x7FFF_FFFF;
        const N: i64 = -0x8000_0000;
        // (TR, column products a, b, c) with V = (1, 1, 1).
        const CASES: [(i64, i32, i32, i32); 10] = [
            (P, 4095, 0, 0),
            (P, 4096, 0, 0),
            (P, 4097, 0, 0),
            (P, 4096, -1, 0),
            (P, 0, 4096, -2),
            (P, 0x7FFF, -0x8000, 0x7FFF),
            (N, 0, 0, 0),
            (N, -1, 0, 0),
            (N, -1, 1, 0),
            (N, 0, -0x8000, 0x7FFF),
        ];
        for sf in [false, true] {
            for lm in [false, true] {
                for (mx, base, cv, add) in [(0u32, cr::RT, 0u32, cr::TRX), (1, cr::LLM, 1, cr::RBK), (2, cr::LCM, 1, cr::RBK)] {
                    for i in 0..CASES.len() {
                        let mut st = self.plausible();
                        let mut m = ZERO;
                        let mut t = [0i64; 3];
                        for row in 0..3 {
                            let (tr, a, b, c) = CASES[(i + 3 * row) % CASES.len()];
                            t[row] = tr;
                            m[row] = [a, b, c];
                        }
                        st.mat(base, m);
                        st.ctl3(add, t);
                        st.vec(0, [1, 1, 1]);
                        self.push(&st, mvmva(sf, mx, 0, cv, lm));
                    }
                }
            }
            // RTPS/RTPT accumulate the same way through RT and TR.
            for i in 0..CASES.len() {
                let mut st = self.plausible();
                let mut m = ZERO;
                let mut t = [0i64; 3];
                for row in 0..3 {
                    let (tr, a, b, c) = CASES[(i + 3 * row) % CASES.len()];
                    t[row] = tr;
                    m[row] = [a, b, c];
                }
                st.mat(cr::RT, m);
                st.ctl3(cr::TRX, t);
                for n in 0..3 {
                    st.vec(n, [1, 1, 1]);
                }
                let lm = self.bool();
                let c = if i % 2 == 0 { op("RTPS", sf, lm) } else { op("RTPT", sf, lm) };
                self.push(&st, c);
            }
        }
        // GPL with sf=1: MAC SHL 12 + IR*IR0 overflows 44 bits. Sums of
        // exactly 2^43-1 are disputed (see `disputed`) and avoided here.
        for lm in [false, true] {
            for (mac, ir0, ir) in [(P, 1, 4094), (P, 1, 4096), (P, 2, 0x7FFF), (N, 1, -1), (N, 1, 0), (N, -0x8000, 0x7FFF), (0x7FFF_F000, 0x1000, 0x7FFF)] {
                let mut st = self.plausible();
                for n in 0..3 {
                    st.d[dr::MAC1 + n] = mac as u32;
                }
                st.d[dr::IR0] = ir0 as u32;
                st.ir([ir, ir - 2, ir.wrapping_neg()]);
                self.push(&st, op("GPL", true, lm));
                self.push(&st, op("GPL", false, lm));
            }
        }
    }

    /// Colour FIFO components at 0 and FFh: GPF (MAC = IR with IR0 = 1000h,
    /// sf=1) and CC/NCCS's colour stage (BK chooses IR, RGBC scales it).
    fn sat_color(&mut self) {
        const IR: [i32; 14] = [-17, -16, -15, -1, 0, 15, 16, 0xFEF, 0xFF0, 0xFFF, 0x1000, 0x1010, 0x7FFF, -0x8000];
        for lm in [false, true] {
            for j in 0..IR.len() {
                let mut st = self.plausible();
                st.d[dr::IR0] = 0x1000;
                st.ir([IR[j], IR[(j + 4) % IR.len()], IR[(j + 9) % IR.len()]]);
                self.push(&st, op("GPF", true, lm));
            }
            for j in 0..IR.len() {
                let mut st = self.plausible();
                st.mat(cr::LCM, ZERO);
                st.ctl3(cr::RBK, [IR[j] as i64, IR[(j + 4) % IR.len()] as i64, IR[(j + 9) % IR.len()] as i64]);
                st.d[dr::RGBC] = self.pick(&[0x00FF_80FF, 0x0080_FF01, 0x2C01_FFFF, 0x3080_8080]);
                self.push(&st, op("CC", true, lm));
                st.ir([0, 0, 0]);
                self.push(&st, op("CDP", true, lm));
            }
        }
    }

    /// SZ3 at 0 and FFFFh, and RTPS's IR3 flag quirk (FLAG.22 from MAC3
    /// SAR 12 as if lm=0, whatever sf and lm are).
    fn sat_rtps_z(&mut self) {
        const Z: [i64; 16] = [-0x8001, -0x8000, -1, 0, 1, 0x7FFF, 0x8000, 0xFFFE, 0xFFFF, 0x1_0000, 0x1_0001, 0x7_FFFF, 0x8_0000, -0x8_0000, 0x7FFF_FFFF, -0x8000_0000];
        for sf in [false, true] {
            for lm in [false, true] {
                for &z in Z.iter() {
                    let mut st = self.plausible();
                    st.mat(cr::RT, IDENTITY);
                    // MAC3 SAR 12 = TRZ + VZ0, with VZ0 small.
                    let vz = self.int(-0x40, 0x40);
                    let trz = (z - vz).clamp(-0x8000_0000, 0x7FFF_FFFF);
                    st.c[cr::TRX + 2] = trz as u32;
                    let mut v = [self.int(-0x100, 0x100) as i32, self.int(-0x100, 0x100) as i32, vz as i32];
                    if self.rng.chance(3) {
                        v[2] = 0;
                        st.c[cr::TRX + 2] = z.clamp(-0x8000_0000, 0x7FFF_FFFF) as u32;
                    }
                    st.vec(0, v);
                    self.push(&st, op("RTPS", sf, lm));
                }
                // sf=0 with the 44-bit sum just past 32 bits: MAC3 wraps,
                // SZ3 and the IR3 flag see the full sum.
                for s in [0x7FFF_FFFFi64, 0x8000_0000, 0x8000_1000, -0x8000_0001, 0xFFFF_F000, 0x1_0000_0000] {
                    let mut st = self.plausible();
                    st.mat(cr::RT, IDENTITY);
                    let (t, r) = Self::split(s);
                    st.c[cr::TRX + 2] = t as u32;
                    let mut m = IDENTITY;
                    m[2] = [0, 0, r];
                    st.mat(cr::RT, m);
                    st.vec(0, [0, 0, 1]);
                    self.push(&st, op("RTPS", sf, lm));
                }
            }
        }
    }

    /// SX2/SY2 at -400h and 3FFh, and MAC0 overflowing in OF + IR*q.
    fn sat_rtps_xy(&mut self) {
        const S: [i64; 8] = [-0x401, -0x400, -0x3FF, 0, 0x3FE, 0x3FF, 0x400, 0x401];
        for j in 0..S.len() {
            for k in 0..3 {
                let mut st = self.plausible();
                st.mat(cr::RT, ZERO);
                st.ctl3(cr::TRX, [0, 0, self.int(0x100, 0x8000)]);
                let frac = if k == 0 { 0 } else { self.int(0, 0xFFFF) };
                st.c[cr::OFX] = (S[j] * 0x1_0000 + frac) as u32;
                st.c[cr::OFY] = (S[(j + 3 + k) % S.len()] * 0x1_0000 + frac) as u32;
                let lm = self.bool();
                self.push(&st, op("RTPS", true, lm));
            }
        }
        // IR1/IR2 at their extremes times the saturated quotient 1FFFFh.
        for (of, ir) in [(0x7FFF_FFFFi64, 0x7FFF), (-0x8000_0000, -0x8000), (0, 0x7FFF), (0, -0x8000), (-0x8000_0000, 0x7FFF), (0x7FFF_FFFF, -0x8000), (0x4000_0000, 0x4000)] {
            let mut st = self.plausible();
            st.mat(cr::RT, IDENTITY);
            st.ctl3(cr::TRX, [ir as i64, -(ir as i64) - 1, 0]);
            st.vec(0, [0, 0, 0]);
            st.c[cr::OFX] = of as u32;
            st.c[cr::OFY] = (-of - 1) as u32;
            st.c[cr::H] = self.int(1, 0xFFFF) as u32;
            self.push(&st, op("RTPS", true, false));
            self.push(&st, op("RTPS", false, false));
        }
    }

    /// IR0 at 0 and 1000h, and MAC0 overflowing in DQB + DQA*q.
    fn sat_rtps_dq(&mut self) {
        const I: [i64; 8] = [-2, -1, 0, 1, 0xFFF, 0x1000, 0x1001, 0x7_FFFF];
        for name in ["RTPS", "RTPT"] {
            for &i in I.iter() {
                let mut st = self.plausible();
                st.c[cr::DQA] = 0;
                st.c[cr::DQB] = (i * 0x1000 + self.int(0, 0xFFF)) as u32;
                self.push(&st, op(name, true, false));
            }
            for (dqa, dqb) in [(0x7FFF, 0x7FFF_FFFFi64), (-0x8000, -0x8000_0000), (0x7FFF, -0x8000_0000), (-0x8000, 0x7FFF_FFFF), (0x7FFF, 0), (-1, 0)] {
                let mut st = self.plausible();
                st.c[cr::DQA] = dqa as u32;
                st.c[cr::DQB] = dqb as u32;
                // Force the division to saturate: SZ3 small.
                st.mat(cr::RT, ZERO);
                st.ctl3(cr::TRX, [0, 0, self.int(0, 3)]);
                self.push(&st, op(name, true, false));
            }
        }
        // RTPT depth-cues its last vertex only: a near vertex whose IR0
        // would saturate flags only when it is V2.
        for k in 0..12 {
            let mut st = self.plausible();
            st.mat(cr::RT, IDENTITY);
            st.ctl3(cr::TRX, [0, 0, 0]);
            st.c[cr::H] = 0x100;
            let near = self.int(1, 0x80) as i32;
            let mut z = [self.int(0x4000, 0x7FFF) as i32, self.int(0x4000, 0x7FFF) as i32, self.int(0x4000, 0x7FFF) as i32];
            z[k % 3] = near;
            for (n, vz) in z.iter().enumerate() {
                let (x, y) = (self.int(-0x100, 0x100) as i32, self.int(-0x100, 0x100) as i32);
                st.vec(n, [x, y, *vz]);
            }
            st.c[cr::DQA] = if k < 6 { -0x80i32 } else { 0x7FFF } as u32;
            st.c[cr::DQB] = if k < 6 { 0x0080_0000 } else { 0x7000_0000 };
            self.push(&st, op("RTPT", true, false));
        }
    }

    /// OTZ at 0 and FFFFh, MAC0 overflowing in ZSF*sum.
    fn sat_avsz(&mut self) {
        for (zsf, sz) in [
            (0x1000, [0x5555, 0x5555, 0x5555, 0]),
            (0x1000, [0x5555, 0x5555, 0x5556, 0]),
            (0x1000, [0x5555, 0x5555, 0x5554, 0]),
            (0x1000, [0x3FFF, 0x4000, 0x4000, 0x4000]),
            (0x1000, [0x4000, 0x4000, 0x4000, 0x4000]),
            (0x1001, [0xFFFF, 0, 0, 0xFFFF]),
            (0x7FFF, [0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF]),
            (0x4000, [0x8000, 0x8000, 0x8000, 0x8000]),
            (0x2AAB, [0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF]),
            (-0x8000, [0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF]),
            (-1, [0, 0, 0, 1]),
            (-1, [0x1000, 0x1000, 0x1000, 0x1000]),
            (0, [0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF]),
            (1, [1, 1, 1, 1]),
        ] {
            let mut st = self.plausible();
            st.c[cr::ZSF3] = zsf as u32;
            st.c[cr::ZSF4] = zsf as u32;
            for (i, z) in sz.iter().enumerate() {
                st.d[dr::SZ0 + (i + 1) % 4] = *z;
            }
            self.push(&st, op("AVSZ3", false, false));
            self.push(&st, op("AVSZ4", false, false));
        }
    }

    /// NCLIP's MAC0 beyond 32 bits, from extreme screen coordinates.
    fn sat_nclip(&mut self) {
        const E: [i32; 5] = [-0x8000, -1, 0, 1, 0x7FFF];
        let mut picked = Vec::new();
        for code in 0..5u32.pow(6) {
            let mut c = code;
            let mut v = [0i64; 6];
            for x in v.iter_mut() {
                *x = E[(c % 5) as usize] as i64;
                c /= 5;
            }
            let [x0, y0, x1, y1, x2, y2] = v;
            let m = x0 * y1 + x1 * y2 + x2 * y0 - x0 * y2 - x1 * y0 - x2 * y1;
            if !(i32::MIN as i64..=i32::MAX as i64).contains(&m) || m == i32::MAX as i64 || m == i32::MIN as i64 + 1 {
                picked.push(v);
            }
        }
        for _ in 0..24.min(picked.len()) {
            let v = picked[self.rng.below(picked.len() as u32) as usize];
            let mut st = self.plausible();
            st.d[dr::SXY0] = pair(v[0] as i32, v[1] as i32);
            st.d[dr::SXY1] = pair(v[2] as i32, v[3] as i32);
            st.d[dr::SXY2] = pair(v[4] as i32, v[5] as i32);
            self.push(&st, op("NCLIP", false, false));
        }
    }

    /// MVMVA with cv=2 (far colour): FC's partial sum at the IR and 44-bit
    /// limits, where only FLAG can show it.
    fn sat_far_color(&mut self) {
        const FC: [i64; 8] = [0x7FFF_FFFF, -0x8000_0000, 0x8, 0x7FFF, 0x8000, -0x8000, -0x8001, 0x10_0000];
        for mx in 0..4 {
            for sf in [false, true] {
                for lm in [false, true] {
                    for j in 0..2 {
                        let mut st = self.state(if j == 0 { Fl::Plausible } else { Fl::Edge });
                        let fc = [FC[(mx as usize * 2 + j) % FC.len()], FC[(mx as usize * 2 + j + 3) % FC.len()], FC[(mx as usize + j + 5) % FC.len()]];
                        st.ctl3(cr::RFC, fc);
                        let v = self.int(0, 3) as u32;
                        self.push(&st, mvmva(sf, mx, v, 2, lm));
                    }
                }
            }
        }
    }

    /// The "MAC+(FC-MAC)*IR0" step: FC far from the colour so the
    /// intermediate IR saturates, and IR0 at its edges.
    fn sat_interpolation(&mut self) {
        const FC: [i64; 6] = [0x7FFF_FFFF, -0x8000_0000, 0x8000, -0x8001, 0xFFF, 0];
        const IR0: [i32; 6] = [0, 0x1000, 0x7FFF, -0x8000, -1, 0x800];
        let names = ["DPCS", "DPCT", "INTPL", "DCPL", "CDP", "NCDS", "NCDT"];
        for (k, name) in names.iter().enumerate() {
            for j in 0..6 {
                let mut st = self.plausible();
                st.ctl3(cr::RFC, [FC[j], FC[(j + 1) % 6], FC[(j + 2) % 6]]);
                st.d[dr::IR0] = IR0[(j + k) % 6] as u32;
                let sf = j % 3 != 0;
                let lm = (j + k) % 2 == 0;
                self.push(&st, op(name, sf, lm));
            }
        }
    }

    // ---- division ---------------------------------------------------------------------

    /// A state where RTPS's quotient is observable: identity rotation, V0
    /// zero, SZ3 = TRZ, and MAC0 = DQB + DQA*q = q.
    fn div_state(&mut self, h: u32, sz3: i64) -> St {
        let mut st = self.plausible();
        st.mat(cr::RT, IDENTITY);
        st.vec(0, [0, 0, 0]);
        let tx = self.int(-0x400, 0x400);
        let ty = self.int(-0x400, 0x400);
        st.ctl3(cr::TRX, [tx, ty, sz3]);
        st.c[cr::H] = h | if self.rng.chance(4) { (self.rng.next_u64() as u32) << 16 } else { 0 };
        st.c[cr::DQA] = 1;
        st.c[cr::DQB] = 0;
        st
    }

    fn division(&mut self) {
        // Every UNR table entry: normalised divisors (SZ3 SHL lzc) at both
        // ends of each entry's range and inside it, at several magnitudes.
        for idx in 0..=0x100u32 {
            let lo = (0x7FC0 + idx * 0x80).max(0x8000);
            let hi = (0x7FC0 + idx * 0x80 + 0x7F).min(0xFFFF);
            if lo > hi {
                continue;
            }
            for k in 0..3 {
                let d = match k {
                    0 => lo,
                    1 => hi,
                    _ => self.int(lo as i64, hi as i64) as u32,
                };
                // SZ3 = d SHR z for a z that keeps it in the same entry.
                let z = self.int(0, 6) as u32;
                let sz3 = (d >> z).max(1);
                let top = (2 * sz3 - 1).min(0xFFFF);
                let h = match self.rng.below(4) {
                    0 => top,
                    1 => self.int(0, (top / 16) as i64) as u32,
                    _ => self.int(0, top as i64) as u32,
                };
                let st = self.div_state(h, sz3 as i64);
                self.push(&st, op("RTPS", true, false));
            }
        }
        // The overflow boundary H >= SZ3*2, SZ3 = 0 and 1.
        let mut pairs: Vec<(u32, u32)> = vec![(0, 0), (1, 0), (0xFFFF, 0), (0, 1), (1, 1), (2, 1), (3, 2), (4, 2)];
        for _ in 0..24 {
            let sz3 = match self.rng.below(3) {
                0 => self.int(1, 0x10) as u32,
                1 => self.int(0x10, 0x7FFF) as u32,
                _ => self.int(0x7FF0, 0x8000) as u32,
            };
            for h in [2 * sz3 - 1, 2 * sz3, 2 * sz3 + 1] {
                if h <= 0xFFFF {
                    pairs.push((h, sz3));
                }
            }
        }
        // Quotients the UNR rounds to 20000h, saturated without the flag.
        pairs.extend([(0xFE3F, 0x7F20), (0xF015, 0x780B)]);
        for _ in 0..40 {
            let sz3 = self.int(0x4000, 0x7FFF) as u32;
            pairs.push((2 * sz3 - 1, sz3));
        }
        // H with bit 15 set (read back sign-extended, used unsigned).
        for _ in 0..16 {
            let h = self.int(0x8000, 0xFFFF) as u32;
            let sz3 = self.int((h / 2 + 1) as i64, 0xFFFF) as u32;
            pairs.push((h, sz3));
        }
        for (h, sz3) in pairs {
            let st = self.div_state(h, sz3 as i64);
            self.push(&st, op("RTPS", true, false));
        }
        // SZ3 from negative and huge Z (saturated before dividing).
        for z in [-1i64, -0x1000, 0x1_0000, 0x7FFF_FFFF] {
            let h = self.int(0, 0xFFFF) as u32;
            let st = self.div_state(h, z);
            self.push(&st, op("RTPS", true, false));
        }
        // RTPT: three divisions; SXY0/SXY1 show the first two quotients.
        for _ in 0..40 {
            let h = self.int(1, 0xFFFF) as u32;
            let mut st = self.div_state(h, 0);
            st.ctl3(cr::TRX, [0x400, -0x400, 0]);
            st.c[cr::OFX] = 0;
            st.c[cr::OFY] = 0;
            for n in 0..3 {
                let vz = self.int(0, 0x7FFF) as i32;
                st.vec(n, [0, 0, vz]);
            }
            self.push(&st, op("RTPT", true, false));
        }
    }

    // ---- FIFOs -------------------------------------------------------------------------

    fn fifo(&mut self) {
        let pushers = [
            "RTPS", "RTPT", "NCS", "NCT", "NCCS", "NCCT", "NCDS", "NCDT", "CC", "CDP", "DCPL", "DPCS", "DPCT", "INTPL", "GPF", "GPL", "NCLIP", "AVSZ3", "AVSZ4",
        ];
        for name in pushers {
            for k in 0..10i32 {
                let mut st = self.plausible();
                // Distinct, recognisable FIFO stages.
                for (i, r) in (dr::SXY0..=dr::SXY2).enumerate() {
                    st.d[r] = pair(0x100 * i as i32 + k, -0x100 * i as i32 - k);
                }
                for (i, r) in (dr::SZ0..=dr::SZ3).enumerate() {
                    st.d[r] = 0x1111 * (i as u32 + 1) + k as u32;
                }
                for (i, r) in (dr::RGB0..=dr::RES1).enumerate() {
                    st.d[r] = 0x1020_3040u32.wrapping_mul(i as u32 + 1) ^ k as u32;
                }
                self.push(&st, op(name, true, k % 2 == 1));
            }
        }
        // Chains: each command's output state is the next one's input, as
        // in a game's transform-light-sort loop.
        let chain = [
            "RTPT", "NCLIP", "AVSZ3", "NCDT", "RTPS", "RTPS", "RTPS", "AVSZ4", "NCCT", "DPCT", "RTPT", "GPF", "GPL", "NCS", "INTPL", "SQR", "OP", "CC", "CDP", "DCPL",
        ];
        for _ in 0..24 {
            let mut st = self.plausible();
            let start = self.rng.below(chain.len() as u32) as usize;
            for step in 0..10 {
                let name = chain[(start + step * 3) % chain.len()];
                let c = op(name, true, self.rng.chance(3));
                self.push(&st, c);
                st = st.after(c);
                // A fresh vertex or two, as a game streams them in.
                if self.bool() {
                    let n = self.rng.below(3) as usize;
                    let v = [self.int(-0x800, 0x800) as i32, self.int(-0x800, 0x800) as i32, self.int(-0x800, 0x800) as i32];
                    st.vec(n, v);
                }
            }
        }
    }

    // ---- LZCS / LZCR ---------------------------------------------------------------

    fn lzcr(&mut self) {
        let mut values: Vec<u32> = vec![0, u32::MAX];
        for k in 0..32 {
            values.extend([1u32 << k, (1u32 << k).wrapping_sub(1), !(1u32 << k), !((1u32 << k).wrapping_sub(1))]);
        }
        for k in 1..32u32 {
            // Exactly k leading zeroes, then exactly k leading ones.
            let low = (self.rng.next_u64() as u32) & ((1u32 << (31 - k)).wrapping_sub(1));
            values.push(1 << (31 - k) | low);
            values.push(!(1 << (31 - k) | low));
        }
        let cmds = ["NCLIP", "AVSZ3", "AVSZ4", "SQR", "OP", "GPF"];
        for (i, v) in values.into_iter().enumerate() {
            let mut st = self.plausible();
            st.d[dr::LZCS] = v;
            self.push(&st, op(cmds[i % cmds.len()], i % 2 == 0, false));
        }
    }

    // ---- IRGB / ORGB -----------------------------------------------------------------

    fn orgb(&mut self) {
        const IR: [i32; 16] = [-0x8000, -0x81, -0x80, -0x7F, -1, 0, 1, 0x7F, 0x80, 0x81, 0xF7F, 0xF80, 0xFFF, 0x1000, 0x1001, 0x7FFF];
        let quiet = ["NCLIP", "AVSZ3", "AVSZ4"];
        for i in 0..64 {
            let mut st = self.plausible();
            st.ir([IR[i % 16], IR[(i * 5 + 3) % 16], IR[(i * 7 + 11) % 16]]);
            if i % 4 == 3 {
                // Garbage above the 16 bits written.
                for r in dr::IR1..=dr::IR3 {
                    st.d[r] = st.d[r] & 0xFFFF | (self.rng.next_u64() as u32) << 16;
                }
            }
            self.push(&st, op(quiet[i % 3], false, false));
        }
        // Commands that write IR1..3: ORGB follows the saturated results.
        for name in ["GPF", "SQR", "OP", "INTPL", "CC", "NCS"] {
            for _ in 0..12 {
                let st = self.state(Fl::Edge);
                let sf = self.bool();
                let lm = self.bool();
                self.push(&st, op(name, sf, lm));
            }
        }
        for _ in 0..64 {
            let mut st = self.plausible();
            let v = [self.int(-0x200, 0x1200) as i32, self.int(-0x200, 0x1200) as i32, self.int(-0x200, 0x1200) as i32];
            st.ir(v);
            let name = quiet[self.rng.below(3) as usize];
            self.push(&st, op(name, false, false));
        }
    }

    // ---- register read/write behaviour ------------------------------------------------

    /// Garbage in every register's ignored bits, read back after commands
    /// that leave most registers alone: 16-bit registers keep their low
    /// half and read sign- or zero-extended (H included), FLAG's input is
    /// cleared, RES1 and the FIFO stages keep all 32 bits.
    fn registers(&mut self) {
        let quiet = ["NCLIP", "AVSZ3", "AVSZ4"];
        for i in 0..150 {
            let st = self.state(Fl::Wide);
            self.push(&st, op(quiet[i % 3], i % 2 == 0, i % 4 < 2));
        }
        for i in 0..50 {
            let mut st = self.plausible();
            st.c[cr::H] = self.int(0x8000, 0xFFFF) as u32 | (self.rng.next_u64() as u32) << 16;
            for r in [4usize, 12, 20, cr::DQA, cr::ZSF3, cr::ZSF4] {
                st.c[r] = st.c[r] & 0xFFFF | (self.rng.next_u64() as u32) << 16;
            }
            st.c[cr::FLAG] = self.rng.next_u64() as u32;
            let name = ["RTPS", "AVSZ3", "AVSZ4", "NCDS", "MVMVA"][i % 5];
            let c = if name == "MVMVA" { mvmva(true, 0, 0, 0, false) } else { op(name, true, false) };
            self.push(&st, c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::MemoryRecorder;

    #[test]
    fn generators_are_reproducible_and_large_enough() {
        let a = generate(DEFAULT_SEED);
        let b = generate(DEFAULT_SEED);
        assert_eq!(a.len(), b.len());
        assert!(a.iter().zip(&b).all(|(x, y)| x.vector == y.vector));
        assert!(a.len() >= 3000, "{} vectors", a.len());
        for name in GENERATORS {
            assert!(a.iter().any(|g| g.generator == name), "{name} made nothing");
        }
        let distinct: std::collections::BTreeSet<_> = a.iter().map(|g| g.vector.id()).collect();
        assert!(distinct.len() * 100 >= a.len() * 99, "{} distinct of {}", distinct.len(), a.len());
        // Every documented command with every sf/lm, and every MVMVA field.
        let cmds: std::collections::BTreeSet<u32> = a
            .iter()
            .filter_map(|g| match &g.vector {
                Vector::GteOp(o) if o.cmd & 0x3F == 0x12 => Some(o.cmd & (0x3F | 1 << 10 | 0x7F << 13)),
                Vector::GteOp(o) => Some(o.cmd & (0x3F | 1 << 10 | 1 << 19)),
                _ => None,
            })
            .collect();
        assert_eq!(cmds.len(), 21 * 4 + 256 + ALIASES.len() * 4);
        // No gated vector is a disputed case.
        let disputed: std::collections::BTreeSet<_> = disputed().iter().map(Vector::id).collect();
        assert_eq!(disputed.len(), 6);
        assert!(a.iter().all(|g| !disputed.contains(&g.vector.id())));
        assert_ne!(generate(DEFAULT_SEED ^ 1)[0].vector, a[0].vector);
    }

    #[test]
    fn division_states_expose_the_quotient() {
        let mut g = Gen { rng: Rng::new(1), out: Vec::new() };
        let st = g.div_state(0x1234, 0x2000);
        let Vector::GteOp(o) = st.op(op("RTPS", true, false)) else { unreachable!() };
        let res = run_op(&o);
        let mac0 = u32::from_le_bytes(res[4 * dr::MAC0..4 * dr::MAC0 + 4].try_into().unwrap());
        assert_eq!(mac0, psx_gte::unr_divide(0x1234, 0x2000).0);
    }

    #[test]
    fn hook_canonicalises_and_budgets() {
        let mut rec = MemoryRecorder::default();
        let mut hook = GteHook::new(2);
        let mut g = Gte::new();
        g.mtc2(dr::IR1 as u32, 5);
        g.ctc2(cr::FLAG as u32, 0x7000_0000);
        let rtps = 0x0018_0001;
        assert!(hook.before_exec(&mut rec, &g, rtps).unwrap().is_some());
        assert!(hook.before_exec(&mut rec, &g, rtps | 0x4A00_0000).unwrap().is_none(), "same imm25, same state");
        let mut raw_d = g.read_all().0;
        raw_d[dr::IR1] = 0x1234_0005; // the same register value, written with garbage
        assert!(hook.before_exec_raw(&mut rec, &raw_d, &g.read_all().1, rtps).unwrap().is_none());
        g.mtc2(dr::IR2 as u32, 6);
        assert!(hook.before_exec(&mut rec, &g, rtps).unwrap().is_some());
        g.mtc2(dr::IR2 as u32, 7);
        assert!(hook.before_exec(&mut rec, &g, rtps).unwrap().is_none(), "budget of 2 per command");
        assert_eq!(rec.vectors.len(), 2);
        let Vector::GteOp(o) = &rec.vectors[0] else { unreachable!() };
        assert_eq!(o.ctrl[cr::FLAG], 0);
        assert_eq!(o.cmd, rtps);
    }

    #[test]
    fn device_matches_the_runner_protocol() {
        // SXYP, IRGB, ORGB and LZCR in the vector are not written.
        let mut data = [0u32; 32];
        data[dr::SXY2] = 0x0001_0002;
        data[dr::SXYP] = 0x0009_0009;
        data[dr::IRGB] = 0x7FFF;
        data[dr::IR1] = 0x80;
        let res = run_op(&GteOp { data, ctrl: [0; 32], cmd: 0x0140_0006 });
        let word = |r: usize| u32::from_le_bytes(res[4 * r..4 * r + 4].try_into().unwrap());
        assert_eq!(word(dr::SXY2), 0x0001_0002);
        assert_eq!(word(dr::SXYP), 0x0001_0002);
        assert_eq!(word(dr::IR1), 0x80);
        assert_eq!(word(dr::ORGB), 1);
        assert_eq!(res.len(), wire::GTE_EXEC_RESULT_SIZE as usize);
    }
}
