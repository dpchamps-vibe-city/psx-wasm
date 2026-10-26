//! Synthetic SPU_RUN generators: seeded, one feature each, routed to what
//! the runner can observe (the voice 1 and 3 capture buffers, the register
//! snapshot, SPU RAM the vector wrote).
//!
//! Every vector must be knowable from its own content:
//! - the capture buffers hold only this run's samples: N is at least 612,
//!   so every ring entry was written during the run;
//! - a captured voice is silent before its signal and cut to silence
//!   before the readback ([`B::cut`]), so the entries the readback races
//!   with the SPU's writes read the same either way;
//! - SPU RAM a vector captures or plays from is RAM it wrote;
//! - nothing depends on the noise generator's state (no register resets it)
//!   or on registers SPU_RESET does not reset (1DA0h, 1DBCh, 1DBEh);
//! - [`robust`] confirms the result does not change within the runner's
//!   timing tolerances; a vector that does gets trailing writes that move
//!   its readback, or is dropped.

use super::{Knowable, Timing, robust};
use crate::exact::Rng;
use crate::store::Store;
use crate::vector::{SpuRam, SpuRun, SpuWrite, Vector};
use anyhow::Result;
use psx_spu::reg;

/// Default generator seed ("PSXW", "M6", 1).
pub const DEFAULT_SEED: u64 = 0x5053_5857_4D36_0001;

/// The generators, in order.
pub const GENERATORS: [&str; 9] = ["adpcm", "adpcm-reserved", "loop", "pitch", "attack", "decay", "sustain", "release", "kon"];

/// Where generated samples go in SPU RAM.
const SAMPLES: u32 = 0x2000;
/// Register the trailing timing writes rewrite (voice 23's start address,
/// with the value SPU_RESET leaves).
const DUMMY: (u32, u16) = (0x170 + reg::SSA, 0x0200);

/// One ADPCM block.
pub fn block(nibbles: &[i8; 28], shift: u8, filter: u8, flags: u8) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[0] = (filter << 4) | (shift & 0xF);
    b[1] = flags;
    for i in 0..14 {
        b[2 + i] = (nibbles[2 * i] as u8 & 0xF) | (nibbles[2 * i + 1] as u8 & 0xF) << 4;
    }
    b
}

/// A vector being built.
#[derive(Clone, Default)]
struct B {
    writes: Vec<SpuWrite>,
    ram: Vec<(u32, Vec<u8>)>,
    samples: u32,
    captures: Vec<(u32, u32)>,
}

/// A voice's registers.
#[derive(Clone, Copy)]
struct V {
    pitch: u16,
    ssa: u32,
    adsr1: u16,
    adsr2: u16,
    vol: [u16; 2],
}

/// Attack never steps, sustain never steps: the level stays where it is
/// written.
const HOLD: (u16, u16) = (0x7F00, 0x1FC0);

impl B {
    fn new(samples: u32) -> Self {
        B { samples, ..Default::default() }
    }

    fn w(&mut self, sample: u32, r: u32, value: u16) {
        self.writes.push(SpuWrite { sample, reg: r, value });
    }

    fn voice(&mut self, s: u32, v: usize, x: V) {
        for (off, val) in [
            (reg::VOLL, x.vol[0]),
            (reg::VOLR, x.vol[1]),
            (reg::PITCH, x.pitch),
            (reg::SSA, (x.ssa >> 3) as u16),
            (reg::ADSR1, x.adsr1),
            (reg::ADSR2, x.adsr2),
        ] {
            self.w(s, reg::voice(v, off), val);
        }
    }

    fn kon(&mut self, s: u32, mask: u32) {
        if mask & 0xFFFF != 0 {
            self.w(s, reg::KON, mask as u16);
        }
        if mask >> 16 != 0 {
            self.w(s, reg::KON + 2, (mask >> 16) as u16);
        }
    }

    fn koff(&mut self, s: u32, mask: u32) {
        if mask & 0xFFFF != 0 {
            self.w(s, reg::KOFF, mask as u16);
        }
        if mask >> 16 != 0 {
            self.w(s, reg::KOFF + 2, (mask >> 16) as u16);
        }
    }

    /// Silence voice `v` from the tick after sample `s` on, for good: its
    /// level drops to zero and no envelope phase can raise it again.
    fn cut(&mut self, s: u32, v: usize) {
        self.w(s, reg::voice(v, reg::ADSR1), HOLD.0);
        self.w(s, reg::voice(v, reg::ADSR2), HOLD.1);
        self.w(s, reg::voice(v, reg::ENVX), 0);
    }

    fn ram(&mut self, addr: u32, bytes: Vec<u8>) {
        self.ram.push((addr, bytes));
    }

    /// Capture both voice buffers.
    fn capture_voices(&mut self) {
        self.captures.push((0x800, 0x800));
    }

    fn build(&self, store: &Store) -> Result<SpuRun> {
        let mut writes = self.writes.clone();
        writes.sort_by_key(|w| w.sample);
        let mut ram = Vec::new();
        for (addr, bytes) in &self.ram {
            let mut b = bytes.clone();
            b.resize(b.len().div_ceil(16) * 16, 0);
            ram.push(SpuRam { addr: *addr, bytes: store.put_blob(&b)? });
        }
        Ok(SpuRun { ram, writes, samples: self.samples, captures: self.captures.clone() })
    }
}

/// A generated vector and the generator that made it.
pub struct Generated {
    pub generator: &'static str,
    pub vector: Vector,
}

struct Gen<'a> {
    rng: Rng,
    store: &'a Store,
    timing: Timing,
    out: Vec<Vector>,
    /// Candidates that were not robust with any trailing writes.
    pub dropped: usize,
}

impl Gen<'_> {
    fn int(&mut self, lo: u32, hi: u32) -> u32 {
        self.rng.range(lo, hi)
    }

    fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.rng.below(xs.len() as u32) as usize]
    }

    fn chance(&mut self, one_in: u32) -> bool {
        self.rng.chance(one_in)
    }

    fn nibbles(&mut self) -> [i8; 28] {
        let mut n = [0i8; 28];
        for x in n.iter_mut() {
            *x = self.int(0, 15) as i8 - 8;
        }
        n
    }

    /// Keep `b` if its result is knowable, trying up to 19 trailing writes
    /// (about 41 cycles each) to move its readback off a tick boundary.
    fn keep(&mut self, b: &B) -> Result<bool> {
        for k in 0..20 {
            let mut c = b.clone();
            for _ in 0..k {
                c.w(c.samples, DUMMY.0, DUMMY.1);
            }
            let run = c.build(self.store)?;
            match robust(&run, self.store, &self.timing)?.1 {
                Knowable::Yes => {
                    self.out.push(Vector::SpuRun(run));
                    return Ok(true);
                }
                Knowable::NotWrites => break,
                Knowable::NotReadback => {}
            }
        }
        self.dropped += 1;
        Ok(false)
    }

    /// A run length, and a capture window [kon, cut) that ends before the
    /// readback and starts late enough that the readback cannot reach it.
    fn window(&mut self, min_len: u32, max_len: u32) -> (u32, u32, u32) {
        let n = self.int(640, 1400);
        let cut = n - self.int(4, 24);
        let len = self.int(min_len, max_len.min(400));
        (n, cut - len, cut)
    }

    /// A looping sample of `blocks` random blocks at `addr`: loop start on
    /// the first, end + repeat on the last.
    fn sample(&mut self, blocks: usize, shifts: &[u8], filters: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for i in 0..blocks {
            let mut flags = 0;
            if i == 0 {
                flags |= 4;
            }
            if i == blocks - 1 {
                flags |= 3;
            }
            let n = self.nibbles();
            let shift = self.pick(shifts);
            let filter = self.pick(filters);
            out.extend_from_slice(&block(&n, shift, filter, flags));
        }
        out
    }

    // ---- generators ----------------------------------------------------

    /// ADPCM decoding: shifts 0..12 and filters 0..4 on random data, on
    /// voices 1 and 3 at pitch 1000h with a held level.
    fn adpcm(&mut self, reserved: bool) -> Result<()> {
        let count = if reserved { 120 } else { 200 };
        let (shifts, filters): (Vec<u8>, Vec<u8>) =
            if reserved { ((0..16).collect(), (0..16).collect()) } else { ((0..13).collect(), (0..5).collect()) };
        for _ in 0..count {
            let (n, a, cut) = self.window(120, 400);
            let mut b = B::new(n);
            b.w(0, reg::SPUCNT, 0xC000);
            for (i, v) in [1usize, 3].into_iter().enumerate() {
                let addr = SAMPLES + 0x400 * i as u32;
                let blocks = self.int(2, 8) as usize;
                let mut data = self.sample(blocks, &shifts, &filters);
                if reserved {
                    // Make sure the reserved values appear.
                    for blk in data.chunks_mut(16) {
                        if self.chance(2) {
                            blk[0] = (self.pick(&[5u8, 6, 7, 8, 13, 15]) << 4) | (blk[0] & 0xF);
                        }
                        if self.chance(2) {
                            blk[0] = (blk[0] & 0xF0) | self.pick(&[13u8, 14, 15]);
                        }
                    }
                }
                b.ram(addr, data);
                b.voice(1 + i as u32, v, V { pitch: 0x1000, ssa: addr, adsr1: HOLD.0, adsr2: HOLD.1, vol: [0, 0] });
                b.w(a + 1, reg::voice(v, reg::ENVX), self.pick(&[0x7FFF, 0x4000, 0x1234]));
            }
            b.kon(a, 0b1010);
            b.cut(cut, 1);
            b.cut(cut, 3);
            b.capture_voices();
            self.keep(&b)?;
        }
        Ok(())
    }

    /// Loop flags: random flag bytes on every block (start, end, repeat,
    /// the ignored code 2, the unknown bits), always ending in a jump.
    fn loops(&mut self) -> Result<()> {
        for _ in 0..200 {
            let (n, a, cut) = self.window(150, 400);
            let mut b = B::new(n);
            b.w(0, reg::SPUCNT, 0xC000);
            for (i, v) in [1usize, 3].into_iter().enumerate() {
                let addr = SAMPLES + 0x400 * i as u32;
                let blocks = self.int(2, 10) as usize;
                let mut data = Vec::new();
                for k in 0..blocks {
                    let mut flags = self.int(0, 7) as u8;
                    if self.chance(8) {
                        flags |= (self.int(1, 31) as u8) << 3;
                    }
                    if k == blocks - 1 {
                        flags |= 1; // always jump at the end
                    }
                    let nib = self.nibbles();
                    data.extend_from_slice(&block(&nib, self.int(0, 8) as u8, self.int(0, 4) as u8, flags));
                }
                b.ram(addr, data);
                let adsr2 = if self.chance(2) { HOLD.1 } else { 0x1FC0 | self.int(0, 0x3F) as u16 };
                b.voice(1 + i as u32, v, V { pitch: self.pick(&[0x1000, 0x0800, 0x2000, 0x1800, 0x3FFF]), ssa: addr, adsr1: 0x00FF, adsr2, vol: [0, 0] });
                if self.chance(4) {
                    // Software sets the repeat address before the loop end.
                    let target = addr + 16 * self.int(0, blocks as u32 - 1);
                    b.w(a + self.int(2, 60), reg::voice(v, reg::LSAX), (target >> 3) as u16);
                }
            }
            b.kon(a, 0b1010);
            b.cut(cut, 1);
            b.cut(cut, 3);
            b.capture_voices();
            self.keep(&b)?;
        }
        Ok(())
    }

    /// Pitch: stopped, slow (interpolation indices), fast, and above
    /// 3FFFh (clipped); changes while playing.
    fn pitch(&mut self) -> Result<()> {
        for _ in 0..200 {
            let (n, a, cut) = self.window(150, 400);
            let mut b = B::new(n);
            b.w(0, reg::SPUCNT, 0xC000);
            for (i, v) in [1usize, 3].into_iter().enumerate() {
                let addr = SAMPLES + 0x400 * i as u32;
                let blocks = self.int(3, 12) as usize;
                let data = self.sample(blocks, &[0, 1, 2, 3, 4], &[0, 1, 2, 3, 4]);
                b.ram(addr, data);
                let pitch = match self.int(0, 5) {
                    0 => self.int(0, 0xFF),
                    1 => self.int(0x100, 0xFFF),
                    2 => self.int(0x1000, 0x3FFF),
                    3 => self.int(0x4000, 0xFFFF),
                    4 => self.pick(&[0, 1, 0x3FFF, 0x4000, 0x4001, 0x7FFF, 0x8000, 0xFFFF]),
                    _ => self.int(0, 0xFFFF),
                } as u16;
                b.voice(1 + i as u32, v, V { pitch, ssa: addr, adsr1: HOLD.0, adsr2: HOLD.1, vol: [0, 0] });
                b.w(a + 1, reg::voice(v, reg::ENVX), 0x7FFF);
                for _ in 0..self.int(0, 3) {
                    let at = self.int(a + 2, cut - 1);
                    b.w(at, reg::voice(v, reg::PITCH), self.int(0, 0x4800) as u16);
                }
            }
            b.kon(a, 0b1010);
            b.cut(cut, 1);
            b.cut(cut, 3);
            b.capture_voices();
            self.keep(&b)?;
        }
        Ok(())
    }

    /// A constant sample (DC) at `addr`: the output follows the envelope.
    fn dc(&mut self, b: &mut B, addr: u32) {
        let level = self.pick(&[7i8, 6, 5, -8]);
        b.ram(addr, block(&[level; 28], 0, 0, 7).to_vec());
    }

    /// One ADSR phase: `phase` 0 attack, 1 decay, 2 sustain, 3 release.
    fn adsr(&mut self, phase: u32) -> Result<()> {
        for _ in 0..240 {
            let (n, a, cut) = self.window(200, 400);
            let mut b = B::new(n);
            b.w(0, reg::SPUCNT, 0xC000);
            for (i, v) in [1usize, 3].into_iter().enumerate() {
                let addr = SAMPLES + 0x10 * i as u32;
                self.dc(&mut b, addr);
                let rnd = self.rng.u16();
                let (adsr1, adsr2) = match phase {
                    // Attack: every mode, shift and step; decay and
                    // sustain held at the top.
                    0 => ((rnd & 0xFF00) | 0x00FF, HOLD.1),
                    // Decay: fast linear attack, every decay shift and
                    // sustain level; sustain held.
                    1 => (0x0000 | (rnd & 0x00FF), HOLD.1),
                    // Sustain: fast attack and decay, every sustain mode,
                    // direction, shift and step.
                    2 => (0x0000 | (rnd & 0x000F), (rnd & 0xDFC0) | 0x1F),
                    // Release: anything, key off during the window.
                    _ => (rnd & 0xFFFF, self.rng.u16()),
                };
                b.voice(1 + i as u32, v, V { pitch: 0x1000, ssa: addr, adsr1, adsr2, vol: [0, 0] });
                if phase == 3 {
                    let off = self.int(a + 1, cut - 1);
                    b.koff(off, 1 << v);
                }
            }
            b.kon(a, 0b1010);
            b.cut(cut, 1);
            b.cut(cut, 3);
            b.capture_voices();
            self.keep(&b)?;
        }
        Ok(())
    }

    /// Key on and key off timing, re-keying, ENDX.
    fn kon(&mut self) -> Result<()> {
        for _ in 0..200 {
            let (n, a, cut) = self.window(150, 400);
            let mut b = B::new(n);
            b.w(0, reg::SPUCNT, 0xC000);
            let mut used = 0u32;
            for (i, v) in [1usize, 3, 5, 9, 17].into_iter().enumerate() {
                let addr = SAMPLES + 0x200 * i as u32;
                let blocks = self.int(1, 6) as usize;
                let mut data = self.sample(blocks, &[0, 1, 2], &[0, 1]);
                if self.chance(3) {
                    // A one-shot: end without repeat (release, level 0).
                    let last = data.len() - 16;
                    data[last + 1] = 1;
                }
                b.ram(addr, data);
                b.voice(1 + i as u32, v, V { pitch: self.pick(&[0x1000, 0x0800, 0x3000]), ssa: addr, adsr1: self.rng.u16() & 0x7FFF, adsr2: self.rng.u16() & 0xDFFF, vol: [0, 0] });
                used |= 1 << v;
            }
            let mut t = a;
            while t < cut - 2 {
                let mask = used & self.rng.next_u64() as u32;
                match self.int(0, 3) {
                    0 => b.kon(t, mask),
                    1 => b.koff(t, mask),
                    2 => {
                        b.kon(t, mask);
                        b.koff(t, used & self.rng.next_u64() as u32);
                    }
                    _ => {
                        b.koff(t, mask);
                        b.kon(t, used & self.rng.next_u64() as u32);
                    }
                }
                t += self.int(1, 120);
            }
            b.cut(cut, 1);
            b.cut(cut, 3);
            b.capture_voices();
            self.keep(&b)?;
        }
        Ok(())
    }
}

/// Every synthetic vector for `seed`, generator by generator, and how many
/// candidates each dropped as not knowable.
pub fn generate(store: &Store, seed: u64, timing: &Timing) -> Result<(Vec<Generated>, Vec<(&'static str, usize)>)> {
    let mut out = Vec::new();
    let mut dropped = Vec::new();
    for (k, name) in GENERATORS.iter().enumerate() {
        let mut g = Gen {
            rng: Rng::new(seed ^ (k as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15)),
            store,
            timing: *timing,
            out: Vec::new(),
            dropped: 0,
        };
        match *name {
            "adpcm" => g.adpcm(false)?,
            "adpcm-reserved" => g.adpcm(true)?,
            "loop" => g.loops()?,
            "pitch" => g.pitch()?,
            "attack" => g.adsr(0)?,
            "decay" => g.adsr(1)?,
            "sustain" => g.adsr(2)?,
            "release" => g.adsr(3)?,
            _ => g.kon()?,
        }
        dropped.push((*name, g.dropped));
        out.extend(g.out.into_iter().map(|vector| Generated { generator: name, vector }));
    }
    Ok((out, dropped))
}
