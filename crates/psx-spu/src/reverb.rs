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
    /// Whether the next tick computes the right side.
    right: bool,
    /// The last HIST inputs per side (44.1 kHz), newest at `pos`.
    input: [[i32; HIST]; 2],
    /// The last outputs per side, zero-stuffed to 44.1 kHz.
    output: [[i32; HIST]; 2],
    pos: usize,
}

impl Default for Reverb {
    fn default() -> Self {
        Reverb { cur: 0, right: false, input: [[0; HIST]; 2], output: [[0; HIST]; 2], pos: 0 }
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
    pub(crate) fn step(spu: &mut Spu, in_l: i32, in_r: i32) -> (i32, i32) {
        let rv = &mut spu.reverb;
        rv.pos = (rv.pos + 1) % HIST;
        rv.input[0][rv.pos] = in_l;
        rv.input[1][rv.pos] = in_r;
        rv.output[0][rv.pos] = 0;
        rv.output[1][rv.pos] = 0;
        let side = rv.right as usize;
        let input = Self::fir(&rv.input[side], rv.pos);
        let out = Self::side(spu, side, input);
        let rv = &mut spu.reverb;
        rv.output[side][rv.pos] = out;
        if rv.right {
            let esa = (spu.regs[(reg::ESA / 2) as usize] as u32) << 3;
            rv.cur = esa.max((rv.cur + 2) & (RAM_BYTES - 2));
        }
        rv.right = !rv.right;
        let l = sat(2 * Self::fir(&rv.output[0], rv.pos));
        let r = sat(2 * Self::fir(&rv.output[1], rv.pos));
        let ev = |o: u32| spu.regs[(o / 2) as usize] as i16 as i32;
        (mul(l, ev(reg::EVOLL)), mul(r, ev(reg::EVOLR)))
    }

    /// Byte address of a work-area offset (in 8-byte units) plus `delta`
    /// bytes, relative to the current address, wrapped into ESA..7FFFFh.
    fn addr(spu: &Spu, units: u16, delta: i32) -> u32 {
        let esa = (spu.regs[(reg::ESA / 2) as usize] as u32) << 3;
        let size = RAM_BYTES - esa;
        let rel = ((units as u32) << 3) as i32 + delta;
        let off = (spu.reverb.cur - esa) as i64 + rel as i64;
        esa + off.rem_euclid(size.max(1) as i64) as u32
    }

    fn load(spu: &mut Spu, units: u16, delta: i32) -> i32 {
        let a = Self::addr(spu, units, delta) & !1;
        spu.touch(a);
        spu.ram[(a >> 1) as usize] as i16 as i32
    }

    fn store(spu: &mut Spu, units: u16, v: i32) {
        if spu.cnt() & cnt::REVERB == 0 {
            return;
        }
        let a = Self::addr(spu, units, 0) & !1;
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

        let lin = mul(v_in, input);
        let same = mul(lin + mul(Self::load(spu, d_same, 0), v_wall) - Self::load(spu, m_same, -2), v_iir) + Self::load(spu, m_same, -2);
        let diff = mul(lin + mul(Self::load(spu, d_diff_other, 0), v_wall) - Self::load(spu, m_diff, -2), v_iir) + Self::load(spu, m_diff, -2);
        Self::store(spu, m_same, same);
        Self::store(spu, m_diff, diff);
        let mut out = mul(v_comb1, Self::load(spu, m_comb1, 0))
            + mul(v_comb2, Self::load(spu, m_comb2, 0))
            + mul(v_comb3, Self::load(spu, m_comb3, 0))
            + mul(v_comb4, Self::load(spu, m_comb4, 0));
        let a1 = Self::load(spu, m_apf1.wrapping_sub(d_apf1), 0);
        out -= mul(v_apf1, a1);
        Self::store(spu, m_apf1, out);
        out = mul(sat(out), v_apf1) + a1;
        let a2 = Self::load(spu, m_apf2.wrapping_sub(d_apf2), 0);
        out -= mul(v_apf2, a2);
        Self::store(spu, m_apf2, out);
        out = mul(sat(out), v_apf2) + a2;
        sat(out)
    }
}
