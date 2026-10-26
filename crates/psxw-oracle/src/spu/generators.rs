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
//! - SPU RAM a vector captures, plays from or has the reverb unit read is
//!   RAM it wrote, and a vector that turns reverb writes on sets ESA (which
//!   resets the reverb's current address) first;
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
pub const GENERATORS: [&str; 18] = [
    "adpcm", "adpcm-reserved", "loop", "pitch", "attack", "decay", "sustain", "release", "kon", "pmon", "sweep", "mainvol", "noise", "reverb",
    "capture", "irq", "mute", "registers",
];

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
                Knowable::NotWrites | Knowable::NotHistory => break,
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

    /// A random level for a held voice (never 7FFFh: attack at the top
    /// moves on to decay).
    fn level(&mut self) -> u16 {
        self.pick(&[0x7FFE, 0x4000, 0x1234, 0x0800]) - self.int(0, 0x7F) as u16
    }

    /// Pitch modulation: voice 0 modulates voice 1, voice 2 voice 3, with
    /// every pitch (above 7FFFh too, where psx-spx describes a glitch).
    fn pmon(&mut self) -> Result<()> {
        for _ in 0..200 {
            let (n, a, cut) = self.window(150, 400);
            let mut b = B::new(n);
            b.w(0, reg::SPUCNT, 0xC000);
            for v in 0..4usize {
                let addr = SAMPLES + 0x200 * v as u32;
                let blocks = self.int(2, 8) as usize;
                let data = self.sample(blocks, &[0, 1, 2, 3, 4, 5, 6], &[0, 1, 2, 3, 4]);
                b.ram(addr, data);
                let pitch = if v % 2 == 0 {
                    self.pick(&[0x0200, 0x0400, 0x0800, 0x1000, 0x2000, 0x3FFF]) as u32
                } else {
                    match self.int(0, 3) {
                        0 => self.int(0, 0x3FFF),
                        1 => self.int(0x4000, 0x7FFF),
                        2 => self.int(0x8000, 0xFFFF),
                        _ => self.pick(&[0x1000, 0x2000, 0x4000, 0x7FFF, 0x8000, 0xFFFF]),
                    }
                } as u16;
                b.voice(1, v, V { pitch, ssa: addr, adsr1: HOLD.0, adsr2: HOLD.1, vol: [0, 0] });
                let level = self.level();
                b.w(a + 1, reg::voice(v, reg::ENVX), level);
            }
            let mut pmon = 0u16;
            for bit in [0u16, 1, 2, 3] {
                if self.chance(2) {
                    pmon |= 1 << bit;
                }
            }
            b.w(2, reg::PMON, pmon);
            if self.chance(4) {
                b.w(self.int(a + 2, cut - 1), reg::PMON, self.rng.u16() & 0xF);
            }
            b.kon(a, 0b1111);
            b.cut(cut, 1);
            b.cut(cut, 3);
            b.capture_voices();
            self.keep(&b)?;
        }
        Ok(())
    }

    /// A random sweep mode word (bit 15 set).
    fn sweep_word(&mut self) -> u16 {
        let mut w = 0x8000 | (self.rng.u16() & 0x707F);
        if self.chance(6) {
            w |= self.rng.u16() & 0x0F80;
        }
        w
    }

    /// Volume sweeps on voices, observed in the current-volume registers.
    fn sweeps(&mut self) -> Result<()> {
        for _ in 0..240 {
            let n = self.int(640, 3000);
            let mut b = B::new(n);
            b.w(0, reg::SPUCNT, 0xC000);
            for v in 0..23usize {
                if self.chance(3) {
                    b.w(1, reg::voice(v, reg::VOLL + 2 * self.int(0, 1)), self.rng.u16() & 0x7FFF);
                }
            }
            for _ in 0..self.int(1, 3) {
                let v = self.int(0, 22) as usize;
                let r = reg::voice(v, reg::VOLL + 2 * self.int(0, 1));
                let s0 = self.int(3, n - 300);
                // A sweep that steps every tick zeroes the step counter,
                // which no register write resets.
                b.w(s0 - 1, r, 0x8000);
                b.w(s0, r, self.rng.u16() & 0x7FFF);
                let s1 = self.int(s0 + 2, n - 2);
                let w = self.sweep_word();
                b.w(s1, r, w);
            }
            self.keep(&b)?;
        }
        Ok(())
    }

    /// Main volume (direct and sweeps) and the plain volume registers.
    fn mainvol(&mut self) -> Result<()> {
        for _ in 0..160 {
            let n = self.int(640, 3000);
            let mut b = B::new(n);
            b.w(0, reg::SPUCNT, 0xC000);
            for r in [reg::EVOLL, reg::EVOLR, reg::AVOLL, reg::AVOLR, reg::BVOLL, reg::BVOLR] {
                if self.chance(2) {
                    b.w(1, r, self.rng.u16());
                }
            }
            for r in [reg::MVOLL, reg::MVOLR] {
                let s0 = self.int(3, n - 300);
                b.w(s0 - 1, r, 0x8000);
                b.w(s0, r, self.rng.u16() & 0x7FFF);
                if self.chance(3) {
                    let s1 = self.int(s0 + 2, n - 2);
                    let w = self.sweep_word();
                    b.w(s1, r, w);
                }
            }
            self.keep(&b)?;
        }
        Ok(())
    }

    /// Noise mode on voices that are not captured: their envelopes, ENDX
    /// and repeat addresses (the noise level itself is not knowable: no
    /// register resets the generator); voices 1 and 3 play ADPCM alongside.
    fn noise(&mut self) -> Result<()> {
        for _ in 0..120 {
            let (n, a, cut) = self.window(150, 400);
            let mut b = B::new(n);
            b.w(0, reg::SPUCNT, 0xC000 | (self.rng.u16() & 0x3F00));
            let mut non = 0u32;
            let mut keyed = 0b1010u32;
            for (i, v) in [1usize, 3, 4, 6, 8, 12, 20].into_iter().enumerate() {
                let addr = SAMPLES + 0x200 * i as u32;
                let blocks = self.int(1, 6) as usize;
                let data = self.sample(blocks, &[0, 1, 2], &[0, 1]);
                b.ram(addr, data);
                b.voice(1 + i as u32, v, V { pitch: self.pick(&[0x1000, 0x0800, 0x3000, 0x0123]), ssa: addr, adsr1: self.rng.u16(), adsr2: self.rng.u16() & 0xDFFF, vol: [0, 0] });
                if v != 1 && v != 3 {
                    keyed |= 1 << v;
                    if self.chance(3) {
                        non |= 1 << v;
                    }
                }
            }
            b.w(1, reg::NON, non as u16);
            b.w(1, reg::NON + 2, (non >> 16) as u16);
            b.kon(a, keyed);
            if self.chance(2) {
                b.w(self.int(a + 1, cut), reg::SPUCNT, 0xC000 | (self.rng.u16() & 0x3F00));
            }
            b.cut(cut, 1);
            b.cut(cut, 3);
            b.capture_voices();
            self.keep(&b)?;
        }
        Ok(())
    }

    /// The reverb unit: a work area the vector initialises, random or
    /// preset configurations, voices 1 and 3 (and 5) as input; reverb
    /// writes stop before the readback so the area is read whole.
    fn reverb(&mut self) -> Result<()> {
        const PRESETS: [(u32, [u16; 32]); 3] = [
            (0x26C0, [0x007D, 0x005B, 0x6D80, 0x54B8, 0xBED0, 0x0000, 0x0000, 0xBA80, 0x5800, 0x5300, 0x04D6, 0x0333, 0x03F0, 0x0227, 0x0374, 0x01EF,
                      0x0334, 0x01B5, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x01B4, 0x0136, 0x00B8, 0x005C, 0x8000, 0x8000]),
            (0x1F40, [0x0033, 0x0025, 0x70F0, 0x4FA8, 0xBCE0, 0x4410, 0xC0F0, 0x9C00, 0x5280, 0x4EC0, 0x03E4, 0x031B, 0x03A4, 0x02AF, 0x0372, 0x0266,
                      0x031C, 0x025D, 0x025C, 0x018E, 0x022F, 0x0135, 0x01D2, 0x00B7, 0x018F, 0x00B5, 0x00B4, 0x0080, 0x004C, 0x0026, 0x8000, 0x8000]),
            (0x3C00, [0x0017, 0x0013, 0x70F0, 0x4FA8, 0xBCE0, 0x4510, 0xBEF0, 0x8500, 0x5F80, 0x54C0, 0x0371, 0x02AF, 0x02E5, 0x01DF, 0x02B0, 0x01D7,
                      0x0358, 0x026A, 0x01D6, 0x011E, 0x012D, 0x00B1, 0x011F, 0x0059, 0x01A0, 0x00E3, 0x0058, 0x0040, 0x0028, 0x0014, 0x8000, 0x8000]),
        ];
        for _ in 0..160 {
            let (n, a, cut) = self.window(200, 400);
            let mut b = B::new(n);
            b.w(0, reg::SPUCNT, 0xC000);
            let size = self.pick(&[0x400u32, 0x800, 0x1000, 0x2000, 0x4000]);
            let base = 0x8_0000 - size;
            // The work area, and the two sizes below it: offsets that wrap
            // "negative" (dAPF above mAPF, [m-2] with m = 0) read there (see
            // psx_spu's reverb addressing), so this RAM must be the
            // vector's own too.
            let mut area = vec![0u8; 3 * size as usize];
            if self.chance(2) {
                for x in area.iter_mut() {
                    *x = self.rng.u16() as u8;
                }
            }
            b.ram(base - 2 * size, area);
            let units = (size / 8) as u16;
            let preset = PRESETS.iter().filter(|p| p.0 <= size).collect::<Vec<_>>();
            let regs: Vec<u16> = if !preset.is_empty() && self.chance(3) {
                let p = preset[self.rng.below(preset.len() as u32) as usize];
                p.1.to_vec()
            } else {
                (0..32)
                    .map(|k| match k {
                        // Volumes (vIIR..vAPF2, vLIN, vRIN).
                        2..=9 | 30 | 31 => match self.int(0, 5) {
                            0 => self.pick(&[0x7FFF, 0x8000, 0x8001, 0, 0x4000, 0xC000]),
                            _ => self.rng.u16(),
                        },
                        // Displacements and addresses, in 8-byte units.
                        _ => self.int(0, units as u32 - 1) as u16,
                    })
                    .collect()
            };
            for (k, val) in regs.iter().enumerate() {
                b.w(1 + (k / 8) as u32, reg::REVERB + 2 * k as u32, *val);
            }
            let mut eon = 0u32;
            for (i, v) in [1usize, 3, 5].into_iter().enumerate() {
                let addr = SAMPLES + 0x200 * i as u32;
                let blocks = self.int(2, 6) as usize;
                let data = self.sample(blocks, &[0, 1, 2, 3], &[0, 1, 2]);
                b.ram(addr, data);
                let vol = [self.rng.u16() & 0x7FFF, self.rng.u16() & 0x7FFF];
                b.voice(6 + i as u32, v, V { pitch: self.pick(&[0x1000, 0x0800, 0x1800]), ssa: addr, adsr1: HOLD.0, adsr2: HOLD.1, vol });
                let level = self.level();
                b.w(a + 1, reg::voice(v, reg::ENVX), level);
                if self.chance(4) != (v == 5) {
                    eon |= 1 << v;
                }
            }
            b.w(10, reg::EON, eon as u16);
            // The work area's start (and current address), then reverb on.
            let e0 = self.int(11, a);
            b.w(e0, reg::ESA, (base >> 3) as u16);
            let e1 = self.int(e0, a + 40);
            b.w(e1, reg::SPUCNT, 0xC080);
            b.kon(a, 0b10_1010);
            b.cut(cut, 1);
            b.cut(cut, 3);
            b.cut(cut, 5);
            // Reverb writes off before the readback.
            let off = self.int(cut + 1, n - 2);
            b.w(off, reg::SPUCNT, 0xC000);
            b.captures.push((base, size));
            b.capture_voices();
            self.keep(&b)?;
        }
        Ok(())
    }

    /// The capture buffers themselves: layouts of captures over them (and
    /// the CD buffers, silent), at every buffer position.
    fn capture(&mut self) -> Result<()> {
        for _ in 0..100 {
            let (n, a, cut) = self.window(100, 400);
            let mut b = B::new(n);
            b.w(0, reg::SPUCNT, 0xC000);
            for (i, v) in [1usize, 3].into_iter().enumerate() {
                let addr = SAMPLES + 0x400 * i as u32;
                let blocks = self.int(2, 8) as usize;
                let data = self.sample(blocks, &[0, 1, 2, 3, 4], &[0, 1, 2, 3, 4]);
                b.ram(addr, data);
                b.voice(1 + i as u32, v, V { pitch: self.pick(&[0x1000, 0x0800, 0x0C00, 0x2000]), ssa: addr, adsr1: HOLD.0, adsr2: HOLD.1, vol: [0, 0] });
                let level = self.level();
                b.w(a + 1, reg::voice(v, reg::ENVX), level);
            }
            b.kon(a, 0b1010);
            b.cut(cut, 1);
            b.cut(cut, 3);
            for _ in 0..self.int(1, 4) {
                let start = 16 * self.int(0, 255);
                let len = 16 * self.int(1, (0x1000 - start) / 16);
                b.captures.push((start, len));
            }
            if self.chance(4) {
                // Voice 1's sample: RAM this vector wrote (at least 2 blocks).
                b.captures.push((SAMPLES, 0x20));
            }
            self.keep(&b)?;
        }
        Ok(())
    }

    /// The IRQ address: capture-buffer writes, voices reading ADPCM data
    /// (the silent block every idle voice plays too), enable and
    /// acknowledge, all observed in SPUSTAT bit 6.
    fn irq(&mut self) -> Result<()> {
        for _ in 0..240 {
            let n = self.int(640, 1400);
            let mut b = B::new(n);
            b.w(0, reg::SPUCNT, 0xC000);
            let mode = self.int(0, 3);
            let (irqa, hit) = match mode {
                0 => {
                    // A capture buffer: index i is written by run tick m with
                    // (256 + m) mod 512 == i.
                    let region = self.pick(&[0x000u32, 0x400, 0x800, 0xC00]);
                    let m = self.int(40, n - 30);
                    let idx = (256 + m) % 512;
                    ((region + 2 * idx) >> 3, m)
                }
                1 => {
                    // Voice 1 reading its sample.
                    let blocks = self.int(2, 8) as usize;
                    let data = self.sample(blocks, &[0, 1, 2], &[0, 1]);
                    b.ram(SAMPLES, data);
                    b.voice(1, 1, V { pitch: self.pick(&[0x1000, 0x0800, 0x2000, 0x0400]), ssa: SAMPLES, adsr1: HOLD.0, adsr2: HOLD.1, vol: [0, 0] });
                    let a = self.int(20, n - 300);
                    b.kon(a, 0b10);
                    let word = self.int(0, 8 * blocks as u32 - 1);
                    ((SAMPLES + 2 * word) >> 3, a + 8 + 4 * word)
                }
                2 => (0x0200, self.int(20, n - 30)),
                _ => (self.int(0, 0xFFFF), self.int(20, n - 30)),
            };
            // The reverb unit reads its work area even while reverb is off;
            // after SPU_RESET that is all of SPU RAM, at an address that
            // depends on how long ago the reset was. Confine it to the last
            // 16 bytes (writing ESA also resets the current address).
            b.w(1, reg::ESA, 0xFFFE);
            b.w(1, reg::IRQA, irqa as u16);
            let enable = if self.chance(2) { hit.saturating_sub(self.int(0, 12)).max(2) } else { self.int(2, n - 4) };
            b.w(enable, reg::SPUCNT, 0xC040);
            if self.chance(4) {
                let ack = self.int(enable, n - 2);
                b.w(ack, reg::SPUCNT, 0xC000);
                if self.chance(2) {
                    b.w(self.int(ack, n - 1), reg::SPUCNT, 0xC040);
                }
            }
            self.keep(&b)?;
        }
        Ok(())
    }

    /// SPUCNT's mute and enable bits while voices 1 and 3 play.
    fn mute(&mut self) -> Result<()> {
        for _ in 0..100 {
            let (n, a, cut) = self.window(150, 400);
            let mut b = B::new(n);
            b.w(0, reg::SPUCNT, 0xC000);
            for (i, v) in [1usize, 3].into_iter().enumerate() {
                let addr = SAMPLES + 0x400 * i as u32;
                let blocks = self.int(2, 6) as usize;
                let data = self.sample(blocks, &[0, 1, 2], &[0, 1]);
                b.ram(addr, data);
                b.voice(1 + i as u32, v, V { pitch: 0x1000, ssa: addr, adsr1: 0x00FF, adsr2: self.pick(&[HOLD.1, 0x0FC0, 0x1F80]), vol: [0x3FFF, 0x3FFF] });
            }
            b.kon(a, 0b1010);
            let mut t = a + 1;
            while t < cut - 2 {
                b.w(t, reg::SPUCNT, self.pick(&[0xC000, 0x8000, 0x4000, 0x0000]));
                t += self.int(1, 80);
            }
            b.w(cut - 1, reg::SPUCNT, 0xC000);
            b.cut(cut, 1);
            b.cut(cut, 3);
            b.capture_voices();
            self.keep(&b)?;
        }
        Ok(())
    }

    /// Register read-back: every register SPU_RESET resets, written with
    /// random values (keys and IRQs kept out of the way).
    fn registers(&mut self) -> Result<()> {
        for _ in 0..160 {
            let n = self.int(640, 1000);
            let mut b = B::new(n);
            b.w(0, reg::SPUCNT, 0xC000);
            for _ in 0..self.int(5, 60) {
                let s = self.int(1, n - 1);
                let (r, val) = match self.int(0, 9) {
                    0 => (reg::voice(self.int(0, 22) as usize, self.pick(&[reg::PITCH, reg::ADSR1, reg::ADSR2, reg::LSAX])), self.rng.u16()),
                    1 => (reg::voice(self.int(0, 22) as usize, self.pick(&[reg::VOLL, reg::VOLR])), self.rng.u16() & 0x7FFF),
                    2 => (self.pick(&[reg::MVOLL, reg::MVOLR]), self.rng.u16() & 0x7FFF),
                    3 => (self.pick(&[reg::EVOLL, reg::EVOLR, reg::AVOLL, reg::AVOLR, reg::BVOLL, reg::BVOLR]), self.rng.u16()),
                    4 => (self.pick(&[reg::PMON, reg::PMON + 2, reg::NON, reg::NON + 2, reg::EON, reg::EON + 2]), self.rng.u16()),
                    5 => (self.pick(&[reg::ESA, reg::TSA]), self.rng.u16()),
                    6 => (reg::IRQA, self.rng.u16()),
                    7 => (reg::REVERB + 2 * self.int(0, 31), self.rng.u16()),
                    8 => (reg::SPUCNT, 0x8000 | (self.rng.u16() & 0x7F0F)),
                    _ => (self.pick(&[reg::KON, reg::KOFF, reg::KON + 2, reg::KOFF + 2]), 0),
                };
                b.w(s, r, val);
            }
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
            "kon" => g.kon()?,
            "pmon" => g.pmon()?,
            "sweep" => g.sweeps()?,
            "mainvol" => g.mainvol()?,
            "noise" => g.noise()?,
            "reverb" => g.reverb()?,
            "capture" => g.capture()?,
            "irq" => g.irq()?,
            "mute" => g.mute()?,
            _ => g.registers()?,
        }
        dropped.push((*name, g.dropped));
        out.extend(g.out.into_iter().map(|vector| Generated { generator: name, vector }));
    }
    Ok((out, dropped))
}
