//! The reverb unit (psx-spx "SPU Reverb Formula"): a Schroeder reverberator
//! over the work area ESA..7FFFFh, computed at 22.05 kHz with the left and
//! right sides on alternate 44.1 kHz ticks, its input and output resampled
//! through the 39-tap FIR.

use crate::tables::REVERB_FIR;
use crate::{RAM_BYTES, Spu, cnt, reg};

/// Input and output history for the resampling FIR.
const HIST: usize = 64;

#[derive(Clone)]
pub(crate) struct Reverb {
    /// Current buffer address, bytes.
    cur: u32,
    /// The last HIST inputs per side (44.1 kHz), newest at `pos`.
    input: [[i32; HIST]; 2],
    /// The last outputs per side, zero-stuffed to 44.1 kHz.
    output: [[i32; HIST]; 2],
    pos: usize,
}

impl Default for Reverb {
    fn default() -> Self {
        Reverb { cur: 0, input: [[0; HIST]; 2], output: [[0; HIST]; 2], pos: 0 }
    }
}

fn sat(v: i32) -> i32 {
    v.clamp(-0x8000, 0x7FFF)
}

fn mul(a: i32, b: i32) -> i32 {
    (a * b) >> 15
}

impl Reverb {
    pub(crate) fn set_base(&mut self, esa: u16) {
        self.cur = (esa as u32) << 3;
    }

    fn fir(hist: &[i32; HIST], pos: usize) -> i32 {
        let mut acc = 0i64;
        for (k, c) in REVERB_FIR.iter().enumerate() {
            acc += *c as i64 * hist[(pos + HIST - k) % HIST] as i64;
        }
        (acc >> 15) as i32
    }

    /// One tick: push the reverb input (left, right), compute this tick's
    /// side, return the reverb output (left, right) for the mix.
    /// `index` is the capture index this tick writes: oracle: the right side
    /// runs on even indices, the left on odd ones.
    pub(crate) fn step(spu: &mut Spu, index: u32, in_l: i32, in_r: i32) -> (i32, i32) {
        let right = index & 1 == 0;
        let rv = &mut spu.reverb;
        rv.pos = (rv.pos + 1) % HIST;
        rv.input[0][rv.pos] = in_l;
        rv.input[1][rv.pos] = in_r;
        rv.output[0][rv.pos] = 0;
        rv.output[1][rv.pos] = 0;
        let side = right as usize;
        // oracle: the resampled input saturates to 16 bits.
        let input = sat(Self::fir(&rv.input[side], rv.pos));
        let out = Self::side(spu, side, input);
        let rv = &mut spu.reverb;
        rv.output[side][rv.pos] = out;
        if right {
            let esa = (spu.regs[(reg::ESA / 2) as usize] as u32) << 3;
            rv.cur = esa.max((rv.cur + 2) & (RAM_BYTES - 2));
        }
        let l = sat(2 * Self::fir(&rv.output[0], rv.pos));
        let r = sat(2 * Self::fir(&rv.output[1], rv.pos));
        let ev = |o: u32| spu.regs[(o / 2) as usize] as i16 as i32;
        (mul(l, ev(reg::EVOLL)), mul(r, ev(reg::EVOLR)))
    }

    /// Byte address of a work-area offset (in 8-byte units) plus `delta`
    /// bytes from the current address.
    /// oracle: the offset is taken as a 19-bit unsigned byte count and the
    /// sum wraps once: past 7FFFFh it continues at ESA. An offset that is
    /// "negative" (dAPF above mAPF, or [m-2] with m = 0) so lands outside
    /// the work area (below ESA, the sum masked to 19 bits).
    fn addr(spu: &Spu, units: u16, delta: i32) -> u32 {
        let esa = (spu.regs[(reg::ESA / 2) as usize] as u32) << 3;
        let off = ((units as u32) << 3).wrapping_add(delta as u32) & (RAM_BYTES - 1);
        let mut a = spu.reverb.cur + off;
        if a >= RAM_BYTES {
            a = a - RAM_BYTES + esa;
        }
        a & (RAM_BYTES - 2)
    }

    fn load(spu: &mut Spu, units: u16, delta: i32) -> i32 {
        let a = Self::addr(spu, units, delta);
        spu.touch(a);
        spu.ram[(a >> 1) as usize] as i16 as i32
    }

    fn store(spu: &mut Spu, units: u16, v: i32) {
        if spu.cnt() & cnt::REVERB == 0 {
            return;
        }
        let a = Self::addr(spu, units, 0);
        spu.touch(a);
        spu.ram[(a >> 1) as usize] = sat(v) as u16;
    }

    /// One side's computation; returns its output before EVOL.
    fn side(spu: &mut Spu, side: usize, input: i32) -> i32 {
        let r = |o: u32| spu.regs[((reg::REVERB + o) / 2) as usize];
        let v = |o: u32| r(o) as i16 as i32;
        let (d_apf1, d_apf2) = (r(0x00), r(0x02));
        let (v_iir, v_comb1, v_comb2, v_comb3, v_comb4, v_wall, v_apf1, v_apf2) =
            (v(0x04), v(0x06), v(0x08), v(0x0A), v(0x0C), v(0x0E), v(0x10), v(0x12));
        // Per side: same/diff targets and sources, combs, APFs, input volume.
        let s = side as u32 * 2;
        let m_same = r(0x14 + s);
        let m_comb1 = r(0x18 + s);
        let m_comb2 = r(0x1C + s);
        let d_same = r(0x20 + s);
        let m_diff = r(0x24 + s);
        let m_comb3 = r(0x28 + s);
        let m_comb4 = r(0x2C + s);
        let d_diff_other = r(0x30 + (2 - s));
        let m_apf1 = r(0x34 + s);
        let m_apf2 = r(0x38 + s);
        let v_in = v(0x3C + s);

        // oracle: the unit works in half steps: every product is taken
        // >> 14 (1.14 of a sample), the half-step sums are halved (floor)
        // and saturated when stored. Lin/Rin and the wall term form a
        // saturated IIR input first.
        let x = |w: i32| sat((half(w, v_wall) + half(input, v_in)) >> 1);
        let x_same = x(Self::load(spu, d_same, 0));
        let x_diff = x(Self::load(spu, d_diff_other, 0));
        let old_same = Self::load(spu, m_same, -2);
        let old_diff = Self::load(spu, m_diff, -2);
        Self::store(spu, m_same, Self::iir(x_same, old_same, v_iir));
        Self::store(spu, m_diff, Self::iir(x_diff, old_diff, v_iir));
        let combs = half(v_comb1, Self::load(spu, m_comb1, 0))
            + half(v_comb2, Self::load(spu, m_comb2, 0))
            + half(v_comb3, Self::load(spu, m_comb3, 0))
            + half(v_comb4, Self::load(spu, m_comb4, 0));
        // oracle: the all-pass feedback term multiplies by the negated
        // volume, negation saturating (-8000h becomes 7FFFh); the all-pass
        // output takes the saturated stored value.
        let a1 = Self::load(spu, m_apf1.wrapping_sub(d_apf1), 0);
        let s1 = sat((combs + half(neg(v_apf1), a1)) >> 1);
        Self::store(spu, m_apf1, s1);
        let o1 = half(s1, v_apf1) + 2 * a1;
        let a2 = Self::load(spu, m_apf2.wrapping_sub(d_apf2), 0);
        let s2 = sat((o1 + half(neg(v_apf2), a2)) >> 1);
        Self::store(spu, m_apf2, s2);
        let out = half(s2, v_apf2) + 2 * a2;
        sat(out >> 1)
    }

    /// The reflection filter: `x * vIIR + old * (1 - vIIR)`, in half steps.
    /// oracle: with vIIR = -8000h the old sample's factor is -2.0, and an
    /// old sample of -8000h then contributes nothing.
    fn iir(x: i32, old: i32, v_iir: i32) -> i32 {
        let o = if v_iir == -0x8000 {
            if old == -0x8000 { 0 } else { old as i64 * -0x10000 }
        } else {
            old as i64 * (0x8000 - v_iir) as i64
        };
        sat((half(x, v_iir) + (o >> 14) as i32) >> 1)
    }
}

/// A 1.15 product in half steps.
fn half(a: i32, b: i32) -> i32 {
    ((a as i64 * b as i64) >> 14) as i32
}

/// Saturating negation of a 1.15 volume.
fn neg(v: i32) -> i32 {
    (-v).min(0x7FFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflection_in_half_steps() {
        // vIIR 1/2: (x + old) / 2.
        assert_eq!(Reverb::iir(100, -50, 0x4000), 25);
        // vIIR 7FFFh: each product floors on its own half step, so a sample
        // of -1 over an old -1 comes out -2.
        assert_eq!(Reverb::iir(-1, -1, 0x7FFF), -2);
        // vIIR -8000h: the old sample counts -2.0 times, except -8000h.
        assert_eq!(Reverb::iir(0, 1, -0x8000), -2);
        assert_eq!(Reverb::iir(10, -0x8000, -0x8000), -10);
    }

    #[test]
    fn offsets_wrap_once() {
        let mut spu = Spu::new();
        spu.write(reg::ESA, 0xFF00); // 7F800h, 2 KiB
        spu.reverb.cur = 0x7FD00;
        // In the area: past 7FFFFh continues at ESA.
        assert_eq!(Reverb::addr(&spu, 0x20, 0), 0x7FE00);
        assert_eq!(Reverb::addr(&spu, 0x80, 0), 0x7F900);
        // [m-2] with m = 0 is 7FFFEh bytes on: it wraps once, still lies
        // past the area and is masked to 19 bits, below ESA.
        assert_eq!(Reverb::addr(&spu, 0, -2), 0x7F4FE);
    }
}

impl Reverb {
    /// Its state into an FNV-1a hash (`Spu::state_hash`).
    pub(crate) fn hash_into(&self, h: &mut u64) {
        crate::mixer::fnv(h, &self.cur.to_le_bytes());
        for side in self.input.iter().chain(self.output.iter()) {
            for v in side {
                crate::mixer::fnv(h, &v.to_le_bytes());
            }
        }
        crate::mixer::fnv(h, &(self.pos as u32).to_le_bytes());
    }
}
