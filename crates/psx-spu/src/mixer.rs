//! The mixer API (unified adapter layer, phase 1 task 7): the SPU's state
//! in its own terms, for native SDK adapters (D59) that drive the SPU
//! without the register protocol. Every setter lands where the matching
//! register write lands (it is that write, by construction), so a voice
//! programmed here is the voice the register path programs: the same
//! latches (KON/KOFF take effect at the next tick, a written repeat
//! address sticks), the same read-back. Getters read what the registers
//! read.
//!
//! Halfword pairs the hardware splits (the 24-bit voice masks, the
//! volume pairs) are set as a whole here; the SDK always writes both
//! halves together.

use crate::{Spu, reg};

/// A stereo side: 0 left, 1 right.
pub type Side = usize;

/// The 24-bit voice masks (each two registers, low then high).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mask {
    /// Key on (KON): latched until the next tick.
    KeyOn,
    /// Key off (KOFF): latched until the next tick.
    KeyOff,
    /// Pitch modulation by the previous voice (PMON).
    PitchMod,
    /// Noise instead of the sample (NON).
    Noise,
    /// Into the reverb (EON).
    Reverb,
    /// Voices that reached a block's end flag (ENDX): read-only; a write
    /// is kept in the register file and changes nothing.
    End,
}

impl Mask {
    fn reg(self) -> u32 {
        match self {
            Mask::KeyOn => reg::KON,
            Mask::KeyOff => reg::KOFF,
            Mask::PitchMod => reg::PMON,
            Mask::Noise => reg::NON,
            Mask::Reverb => reg::EON,
            Mask::End => reg::ENDX,
        }
    }
}

/// The stereo volumes outside the voices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Volume {
    /// Main volume (or its sweep), MVOLL/MVOLR.
    Main,
    /// Reverb output volume, EVOLL/EVOLR.
    Reverb,
    /// CD input volume, AVOLL/AVOLR.
    Cd,
    /// External input volume, BVOLL/BVOLR.
    External,
}

impl Volume {
    fn reg(self, side: Side) -> u32 {
        let base = match self {
            Volume::Main => reg::MVOLL,
            Volume::Reverb => reg::EVOLL,
            Volume::Cd => reg::AVOLL,
            Volume::External => reg::BVOLL,
        };
        base + 2 * (side as u32 & 1)
    }
}

impl Spu {
    // ---- voices ---------------------------------------------------------

    /// Voice `v`'s volume on `side` as written (a level, or a sweep with
    /// bit 15 set).
    pub fn voice_volume(&self, v: usize, side: Side) -> u16 {
        self.voices[v].vol[side & 1]
    }

    pub fn set_voice_volume(&mut self, v: usize, side: Side, value: u16) {
        self.write(reg::voice(v, reg::VOLL + 2 * (side as u32 & 1)), value);
    }

    pub fn voice_pitch(&self, v: usize) -> u16 {
        self.voices[v].pitch
    }

    pub fn set_voice_pitch(&mut self, v: usize, pitch: u16) {
        self.write(reg::voice(v, reg::PITCH), pitch);
    }

    /// The start address (8-byte units).
    pub fn voice_start(&self, v: usize) -> u16 {
        self.voices[v].ssa
    }

    pub fn set_voice_start(&mut self, v: usize, addr: u16) {
        self.write(reg::voice(v, reg::SSA), addr);
    }

    /// ADSR1 and ADSR2 as written.
    pub fn voice_adsr(&self, v: usize) -> (u16, u16) {
        (self.voices[v].adsr1, self.voices[v].adsr2)
    }

    pub fn set_voice_adsr1(&mut self, v: usize, value: u16) {
        self.write(reg::voice(v, reg::ADSR1), value);
    }

    pub fn set_voice_adsr2(&mut self, v: usize, value: u16) {
        self.write(reg::voice(v, reg::ADSR2), value);
    }

    /// The envelope's current level (ENVX).
    pub fn voice_envelope(&self, v: usize) -> i16 {
        self.voices[v].envx
    }

    /// Set the envelope's level (ENVX is writable).
    pub fn set_voice_envelope(&mut self, v: usize, level: u16) {
        self.write(reg::voice(v, reg::ENVX), level);
    }

    /// The repeat address (8-byte units), as the decoder last set it or
    /// software wrote it.
    pub fn voice_repeat(&self, v: usize) -> u16 {
        self.voices[v].lsa
    }

    /// Write the repeat address: it sticks until the key-on hold ends.
    pub fn set_voice_repeat(&mut self, v: usize, addr: u16) {
        self.write(reg::voice(v, reg::LSAX), addr);
    }

    /// The voice's current volume on `side` (1E00h + 4*voice).
    pub fn voice_current_volume(&self, v: usize, side: Side) -> i16 {
        self.voices[v].volx[side & 1]
    }

    // ---- masks, volumes, control ----------------------------------------

    /// A voice mask as its registers read: KON/KOFF read what was written,
    /// ENDX the voices that ended.
    pub fn mask_bits(&self, m: Mask) -> u32 {
        let r = m.reg();
        self.read(r) as u32 | (self.read(r + 2) as u32 & 0xFF) << 16
    }

    /// Set a voice mask (bits 0-23): both of its registers, low first.
    pub fn set_mask_bits(&mut self, m: Mask, bits: u32) {
        let r = m.reg();
        self.write(r, bits as u16);
        self.write(r + 2, (bits >> 16) as u16 & 0xFF);
    }

    /// One of a mask's registers alone (`high`: voices 16-23), as written.
    pub fn set_mask_half(&mut self, m: Mask, high: bool, value: u16) {
        self.write(m.reg() + if high { 2 } else { 0 }, value);
    }

    /// One of a mask's registers as it reads.
    pub fn mask_half(&self, m: Mask, high: bool) -> u16 {
        self.read(m.reg() + if high { 2 } else { 0 })
    }

    pub fn common_volume(&self, which: Volume, side: Side) -> u16 {
        self.read(which.reg(side))
    }

    pub fn set_common_volume(&mut self, which: Volume, side: Side, value: u16) {
        self.write(which.reg(side), value);
    }

    /// The main volume's current level (1DB8h/1DBAh).
    pub fn main_current_volume(&self, side: Side) -> i16 {
        self.main_volx[side & 1]
    }

    /// SPUCNT: enable, mute, noise clock, reverb, IRQ, transfer mode, CD
    /// and external input.
    pub fn control(&self) -> u16 {
        self.read(reg::SPUCNT)
    }

    /// Write SPUCNT: with transfer mode 1, the FIFO goes to SPU RAM; with
    /// the IRQ disabled, its flag clears.
    pub fn set_control(&mut self, value: u16) {
        self.write(reg::SPUCNT, value);
    }

    /// The transfer control register (1DACh).
    pub fn transfer_control(&self) -> u16 {
        self.read(reg::RAMCNT)
    }

    pub fn set_transfer_control(&mut self, value: u16) {
        self.write(reg::RAMCNT, value);
    }

    /// The transfer address register (8-byte units) as written; the
    /// internal address moves on with every transfer.
    pub fn transfer_start(&self) -> u16 {
        self.read(reg::TSA)
    }

    /// Set the transfer address: transfers start there.
    pub fn set_transfer_start(&mut self, addr: u16) {
        self.write(reg::TSA, addr);
    }

    /// A halfword into the transfer FIFO (the manual write port); transfer
    /// mode 1 sends it to SPU RAM.
    pub fn push_transfer(&mut self, h: u16) {
        self.write(reg::DATA, h);
    }

    /// The reverb work area's start (ESA, 8-byte units).
    pub fn reverb_base(&self) -> u16 {
        self.read(reg::ESA)
    }

    pub fn set_reverb_base(&mut self, addr: u16) {
        self.write(reg::ESA, addr);
    }

    /// The SPU IRQ address (8-byte units).
    pub fn irq_address(&self) -> u16 {
        self.read(reg::IRQA)
    }

    pub fn set_irq_address(&mut self, addr: u16) {
        self.write(reg::IRQA, addr);
    }

    /// Reverb configuration register `i` (0-31: 1DC0h + 2*i).
    pub fn reverb_param(&self, i: usize) -> u16 {
        self.read(reg::REVERB + 2 * (i as u32 & 31))
    }

    pub fn set_reverb_param(&mut self, i: usize, value: u16) {
        self.write(reg::REVERB + 2 * (i as u32 & 31), value);
    }

    // ---- observation ------------------------------------------------------

    /// Everything the SPU will do from here on depends on, hashed
    /// (FNV-1a): the register file, every voice's state (envelope,
    /// decoder, pitch counter, the key-on hold), the key latches, ENDX,
    /// the transfer address and FIFO, the IRQ flag, the capture index, the
    /// noise generator, the reverb unit and SPU RAM. Conformance compares
    /// it between a native adapter and the original over the registers
    /// (D63): both must leave the SPU in the same state.
    pub fn state_hash(&self) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        let w = |h: &mut u64, v: u64| fnv(h, &v.to_le_bytes());
        for r in self.regs.iter() {
            w(&mut h, *r as u64);
        }
        for v in self.voices.iter() {
            for x in [
                v.vol[0] as u64,
                v.vol[1] as u64,
                v.volx[0] as u16 as u64,
                v.volx[1] as u16 as u64,
                v.vol_counter[0] as u64,
                v.vol_counter[1] as u64,
                v.pitch as u64,
                v.ssa as u64,
                v.adsr1 as u64,
                v.adsr2 as u64,
                v.envx as u16 as u64,
                v.env_counter as u64,
                v.phase as u64,
                v.lsa as u64,
                v.addr as u64,
                v.counter,
                v.decoded,
                v.shift as u64,
                v.filter as u64,
                v.flags as u64,
                v.prev[0] as u32 as u64,
                v.prev[1] as u32 as u64,
                v.delay as u64,
                v.kon_age as u64,
                v.lsa_locked as u64,
                v.out as u32 as u64,
            ] {
                w(&mut h, x);
            }
            for s in v.ring.iter() {
                w(&mut h, *s as u16 as u64);
            }
        }
        for x in [
            self.main_volx[0] as u16 as u64,
            self.main_volx[1] as u16 as u64,
            self.main_counter[0] as u64,
            self.main_counter[1] as u64,
            self.kon as u64,
            self.koff as u64,
            self.endx as u64,
            self.transfer_addr as u64,
            self.fifo_len as u64,
            self.irq_flag as u64,
            self.capture as u64,
            self.noise.level as u64,
            self.noise.timer as u32 as u64,
            self.cd_in[0] as u16 as u64,
            self.cd_in[1] as u16 as u64,
        ] {
            w(&mut h, x);
        }
        for f in self.fifo[..self.fifo_len].iter() {
            w(&mut h, *f as u64);
        }
        self.reverb.hash_into(&mut h);
        for r in self.ram.iter() {
            fnv(&mut h, &r.to_le_bytes());
        }
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A voice and the common state programmed through the API equal
    /// the same programmed through the registers, tick after tick.
    #[test]
    fn api_reaches_the_register_state() {
        let mut a = Spu::new();
        let mut b = Spu::new();
        // A looping block of a loud square wave at 0x1000.
        for spu in [&mut a, &mut b] {
            spu.write(reg::TSA, 0x200);
            spu.transfer_write(0x0400);
            for _ in 0..7 {
                spu.transfer_write(0x7788);
            }
            spu.transfer_write(0x0300);
            for _ in 0..7 {
                spu.transfer_write(0x8877);
            }
            spu.write(reg::SPUCNT, 0xC000);
        }
        a.set_common_volume(Volume::Main, 0, 0x3FFF);
        a.set_common_volume(Volume::Main, 1, 0x3FFF);
        a.set_voice_volume(1, 0, 0x3FFF);
        a.set_voice_volume(1, 1, 0x2000);
        a.set_voice_pitch(1, 0x1000);
        a.set_voice_start(1, 0x200);
        a.set_voice_adsr1(1, 0x000F);
        a.set_voice_adsr2(1, 0x0000);
        a.set_mask_bits(Mask::Reverb, 0x2);
        a.set_mask_bits(Mask::KeyOn, 0x2);
        b.write(reg::MVOLL, 0x3FFF);
        b.write(reg::MVOLR, 0x3FFF);
        b.write(reg::voice(1, reg::VOLL), 0x3FFF);
        b.write(reg::voice(1, reg::VOLR), 0x2000);
        b.write(reg::voice(1, reg::PITCH), 0x1000);
        b.write(reg::voice(1, reg::SSA), 0x200);
        b.write(reg::voice(1, reg::ADSR1), 0x000F);
        b.write(reg::voice(1, reg::ADSR2), 0x0000);
        b.write(reg::EON, 0x2);
        b.write(0x19A, 0);
        b.write(reg::KON, 0x2);
        b.write(0x18A, 0);
        for t in 0..2000 {
            assert_eq!(a.tick(), b.tick(), "tick {t}");
            if t == 1000 {
                a.set_mask_bits(Mask::KeyOff, 0x2);
                b.write(reg::KOFF, 0x2);
                b.write(0x18E, 0);
            }
        }
        assert_eq!(a.voice_envelope(1), b.read(reg::voice(1, reg::ENVX)) as i16);
        assert_eq!(a.mask_bits(Mask::End), b.read(reg::ENDX) as u32 | (b.read(0x19E) as u32) << 16);
        assert_eq!(a.voice_repeat(1), b.read(reg::voice(1, reg::LSAX)));
        for off in (0..0x200).step_by(2) {
            assert_eq!(a.read(off), b.read(off), "register {:#x}", 0x1C00 + off);
        }
        assert_eq!(a.state_hash(), b.state_hash());
    }
}

/// FNV-1a over `bytes`.
pub(crate) fn fnv(h: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *h = (*h ^ b as u64).wrapping_mul(0x100_0000_01b3);
    }
}
