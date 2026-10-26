//! The PlayStation SPU at register level: 512 KiB of SPU RAM, the register
//! file with its read-back behaviour, 24 ADPCM voices (pitch, pitch
//! modulation, 4-point Gaussian interpolation, ADSR, volume sweeps, noise),
//! key on/off and ENDX, mixing and main volume, the reverb unit over its
//! work area, the capture buffers, the IRQ address, and SPU RAM transfers
//! (the manual FIFO and DMA, as a device API).
//!
//! The specification is psx-spx, "Sound Processing Unit (SPU)". Where it is
//! silent or ambiguous, the device oracle (`psxw oracle check <game>
//! --device spu`, emulator goldens) settled the behaviour; those places are
//! marked "oracle:" and recorded in plans/M06-sound.md (Findings).
//!
//! Time: [`Spu::tick`] produces one 44.1 kHz sample (768 CPU cycles).
//! Register writes made between two ticks take effect at the next tick,
//! as on hardware ("the SPU seems to process written values at 44100Hz
//! rate"). Register reads see the state after the last tick.
//!
//! `no_std` + `alloc`: the runtime runs it in wasm32; the oracle natively.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec;

pub mod envelope;
mod reverb;
pub mod tables;

use envelope::Rate;
use tables::{FILTER_NEG, FILTER_POS, GAUSS};

/// SPU RAM size in bytes.
pub const RAM_BYTES: u32 = 0x8_0000;
/// Voices.
pub const VOICES: usize = 24;
/// CPU cycles per 44.1 kHz sample.
pub const CYCLES_PER_SAMPLE: u32 = 768;
/// Samples per capture buffer (each is 1 KiB of 16-bit samples).
pub const CAPTURE_SAMPLES: u32 = 0x200;

/// Register offsets from 1F801C00h.
pub mod reg {
    /// Voice register `off` (0..0Eh) of voice `v`.
    pub const fn voice(v: usize, off: u32) -> u32 {
        0x10 * v as u32 + off
    }
    pub const VOLL: u32 = 0x0;
    pub const VOLR: u32 = 0x2;
    pub const PITCH: u32 = 0x4;
    pub const SSA: u32 = 0x6;
    pub const ADSR1: u32 = 0x8;
    pub const ADSR2: u32 = 0xA;
    pub const ENVX: u32 = 0xC;
    pub const LSAX: u32 = 0xE;
    pub const MVOLL: u32 = 0x180;
    pub const MVOLR: u32 = 0x182;
    pub const EVOLL: u32 = 0x184;
    pub const EVOLR: u32 = 0x186;
    pub const KON: u32 = 0x188;
    pub const KOFF: u32 = 0x18C;
    pub const PMON: u32 = 0x190;
    pub const NON: u32 = 0x194;
    pub const EON: u32 = 0x198;
    pub const ENDX: u32 = 0x19C;
    pub const ESA: u32 = 0x1A2;
    pub const IRQA: u32 = 0x1A4;
    pub const TSA: u32 = 0x1A6;
    pub const DATA: u32 = 0x1A8;
    pub const SPUCNT: u32 = 0x1AA;
    pub const RAMCNT: u32 = 0x1AC;
    pub const SPUSTAT: u32 = 0x1AE;
    pub const AVOLL: u32 = 0x1B0;
    pub const AVOLR: u32 = 0x1B2;
    pub const BVOLL: u32 = 0x1B4;
    pub const BVOLR: u32 = 0x1B6;
    pub const MVOLXL: u32 = 0x1B8;
    pub const MVOLXR: u32 = 0x1BA;
    /// Reverb configuration area 1DC0h..1DFFh.
    pub const REVERB: u32 = 0x1C0;
    /// Current voice volumes, 1E00h + 4*voice (L), +2 (R).
    pub const VOLX: u32 = 0x200;
}

/// Bits of SPUCNT.
pub mod cnt {
    pub const ENABLE: u16 = 0x8000;
    pub const UNMUTE: u16 = 0x4000;
    pub const REVERB: u16 = 0x0080;
    pub const IRQ_ENABLE: u16 = 0x0040;
    pub const TRANSFER_MODE: u16 = 0x0030;
    pub const CD_REVERB: u16 = 0x0004;
    pub const CD_ENABLE: u16 = 0x0001;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Phase {
    Attack,
    Decay,
    Sustain,
    #[default]
    Release,
}

/// One voice's state.
#[derive(Clone, Debug, Default)]
pub struct Voice {
    /// VOLL/VOLR as written.
    pub vol: [u16; 2],
    /// Current volumes (1E00h + 4*voice).
    pub volx: [i16; 2],
    vol_counter: [u16; 2],
    pub pitch: u16,
    pub ssa: u16,
    pub adsr1: u16,
    pub adsr2: u16,
    /// The ADSR level (ENVX).
    pub envx: i16,
    env_counter: u16,
    pub phase: Phase,
    /// The repeat address (LSAX), in 8-byte units.
    pub lsa: u16,
    /// The ADPCM block being decoded, byte address.
    pub addr: u32,
    /// Pitch counter since key on: bits 12+ are the sample position, bits
    /// 4..11 the interpolation index.
    pub counter: u64,
    /// Samples decoded since key on.
    decoded: u64,
    /// Decoded samples by position, modulo 32.
    ring: [i16; 32],
    /// The decoding block's header: shift, filter and flags.
    shift: u8,
    filter: u8,
    flags: u8,
    /// ADPCM filter history: the last two decoded samples.
    prev: [i32; 2],
    /// Ticks left before the envelope and the pitch counter start after a
    /// key on.
    delay: u8,
    /// Ticks since the last key on (saturating).
    kon_age: u8,
    /// Software wrote LSAX: loop-start flags no longer replace it.
    lsa_locked: bool,
    /// Output after the envelope, for pitch modulation of the next voice.
    out: i32,
}

/// oracle: after a key on, the voice skips this many envelope and pitch
/// updates (its first sample plays five times).
pub const KEY_ON_DELAY: u8 = 4;

/// oracle: the decoder decodes another data halfword when fewer than this
/// many samples are buffered ahead of the playback position.
pub const DECODE_AHEAD: u64 = 7;

/// The noise generator (psx-spx "SPU Noise Generator").
#[derive(Clone, Copy, Debug, Default)]
struct Noise {
    level: u16,
    timer: i32,
}

/// The SPU.
#[derive(Clone)]
pub struct Spu {
    ram: Box<[u16]>,
    /// Registers as written, for those that read back what was written.
    regs: Box<[u16]>,
    pub voices: [Voice; VOICES],
    /// Current main volume (1DB8h/1DBAh).
    main_volx: [i16; 2],
    main_counter: [u16; 2],
    kon: u32,
    koff: u32,
    endx: u32,
    /// The internal transfer address (bytes), incremented by transfers.
    transfer_addr: u32,
    fifo: [u16; 32],
    fifo_len: usize,
    irq_flag: bool,
    /// Next capture buffer index (0..1FFh).
    capture: u32,
    noise: Noise,
    reverb: reverb::Reverb,
    /// CD audio input for the next tick (left, right).
    cd_in: [i16; 2],
}

impl Default for Spu {
    fn default() -> Self {
        Self::new()
    }
}

fn clamp16(v: i32) -> i32 {
    v.clamp(-0x8000, 0x7FFF)
}

impl Spu {
    /// A powered-on SPU: RAM and registers zero.
    pub fn new() -> Self {
        Spu {
            ram: vec![0u16; (RAM_BYTES / 2) as usize].into_boxed_slice(),
            regs: vec![0u16; 0x200].into_boxed_slice(),
            voices: Default::default(),
            main_volx: [0; 2],
            main_counter: [0; 2],
            kon: 0,
            koff: 0,
            endx: 0,
            transfer_addr: 0,
            fifo: [0; 32],
            fifo_len: 0,
            irq_flag: false,
            capture: 0,
            noise: Noise::default(),
            reverb: reverb::Reverb::default(),
            cd_in: [0; 2],
        }
    }

    /// SPU RAM as halfwords.
    pub fn ram(&self) -> &[u16] {
        &self.ram
    }

    /// SPU RAM bytes `[addr, addr+len)` (little-endian halfwords).
    pub fn ram_bytes(&self, addr: u32, len: u32) -> alloc::vec::Vec<u8> {
        (0..len).map(|i| {
            let a = (addr + i) & (RAM_BYTES - 1);
            (self.ram[(a >> 1) as usize] >> (8 * (a & 1))) as u8
        }).collect()
    }

    fn cnt(&self) -> u16 {
        self.regs[(reg::SPUCNT / 2) as usize]
    }

    fn raw(&self, off: u32) -> u16 {
        self.regs[(off / 2) as usize]
    }

    /// The IRQ line (SPUSTAT bit 6, while IRQs are enabled).
    pub fn irq(&self) -> bool {
        self.irq_flag
    }

    /// Note an SPU RAM access at byte `addr` for the IRQ address.
    /// oracle: only an access to the exact halfword at the IRQ address
    /// triggers (a capture write one halfword later does not).
    fn touch(&mut self, addr: u32) {
        let c = self.cnt();
        if c & cnt::IRQ_ENABLE != 0 && c & cnt::ENABLE != 0 && (addr & !1) == (self.raw(reg::IRQA) as u32) << 3 {
            self.irq_flag = true;
        }
    }

    // ---- registers ------------------------------------------------------

    /// Read the 16-bit register at `off` (1F801C00h + off, off < 400h).
    pub fn read(&self, off: u32) -> u16 {
        let off = off & 0x3FE;
        if off < 0x180 {
            let v = &self.voices[(off >> 4) as usize];
            return match off & 0xF {
                reg::ENVX => v.envx as u16,
                reg::LSAX => v.lsa,
                _ => self.raw(off),
            };
        }
        match off {
            reg::ENDX => self.endx as u16,
            0x19E => (self.endx >> 16) as u16,
            reg::DATA => 0,
            reg::SPUSTAT => self.status(),
            reg::MVOLXL => self.main_volx[0] as u16,
            reg::MVOLXR => self.main_volx[1] as u16,
            0x200..=0x25E => {
                let v = &self.voices[((off - 0x200) >> 2) as usize];
                v.volx[((off >> 1) & 1) as usize] as u16
            }
            _ => self.raw(off),
        }
    }

    /// SPUSTAT.
    pub fn status(&self) -> u16 {
        let c = self.cnt();
        let mode = (c >> 4) & 3;
        let mut s = c & 0x3F;
        if self.irq_flag {
            s |= 0x40;
        }
        // oracle: bit 7 follows the DMA transfer modes; bits 8/9 are the
        // write and read requests.
        if mode >= 2 {
            s |= 0x80;
        }
        if mode == 2 {
            s |= 0x100;
        }
        if mode == 3 {
            s |= 0x200;
        }
        // oracle: bit 11 is the half of the capture index last written,
        // reported only while the transfer control register holds 0004h
        // (a recorded btn window with it 0 read the bit as clear).
        if self.raw(0x1AC) == 0x0004 && self.capture.wrapping_sub(1) & 0x100 != 0 {
            s |= 0x800;
        }
        s
    }

    /// Write the 16-bit register at `off`.
    pub fn write(&mut self, off: u32, value: u16) {
        let off = off & 0x3FE;
        self.regs[(off / 2) as usize] = value;
        if off < 0x180 {
            let v = &mut self.voices[(off >> 4) as usize];
            match off & 0xF {
                reg::VOLL => v.vol[0] = value,
                reg::VOLR => v.vol[1] = value,
                reg::PITCH => v.pitch = value,
                reg::SSA => v.ssa = value,
                reg::ADSR1 => v.adsr1 = value,
                reg::ADSR2 => v.adsr2 = value,
                reg::ENVX => v.envx = value as i16,
                _ => {
                    // oracle: a repeat address software writes sticks;
                    // loop-start flags no longer replace it.
                    v.lsa = value;
                    v.lsa_locked = true;
                }
            }
            return;
        }
        match off {
            // oracle: KON and KOFF latch the last value written before the
            // tick that processes them.
            reg::KON => self.kon = (self.kon & !0xFFFF) | value as u32,
            0x18A => self.kon = (self.kon & 0xFFFF) | (value as u32 & 0xFF) << 16,
            reg::KOFF => self.koff = (self.koff & !0xFFFF) | value as u32,
            0x18E => self.koff = (self.koff & 0xFFFF) | (value as u32 & 0xFF) << 16,
            reg::ESA => self.reverb.set_base(value),
            reg::TSA => self.transfer_addr = (value as u32) << 3,
            reg::DATA => {
                if self.fifo_len < self.fifo.len() {
                    self.fifo[self.fifo_len] = value;
                    self.fifo_len += 1;
                }
            }
            reg::SPUCNT => {
                if value & cnt::IRQ_ENABLE == 0 {
                    self.irq_flag = false;
                }
                if (value & cnt::TRANSFER_MODE) >> 4 == 1 {
                    self.flush_fifo();
                }
            }
            _ => {}
        }
    }

    fn mask(&self, off: u32) -> u32 {
        self.raw(off) as u32 | ((self.raw(off + 2) as u32 & 0xFF) << 16)
    }

    // ---- transfers ----------------------------------------------------------

    /// Manual write: the FIFO goes to SPU RAM at the transfer address.
    fn flush_fifo(&mut self) {
        for i in 0..self.fifo_len {
            let h = self.fifo[i];
            self.transfer_write(h);
        }
        self.fifo_len = 0;
    }

    /// One halfword to SPU RAM at the transfer address (DMA write).
    pub fn transfer_write(&mut self, h: u16) {
        let a = self.transfer_addr & (RAM_BYTES - 1);
        self.touch(a);
        self.ram[(a >> 1) as usize] = h;
        self.transfer_addr = (a + 2) & (RAM_BYTES - 1);
    }

    /// One halfword from SPU RAM at the transfer address (DMA read).
    pub fn transfer_read(&mut self) -> u16 {
        let a = self.transfer_addr & (RAM_BYTES - 1);
        self.touch(a);
        let h = self.ram[(a >> 1) as usize];
        self.transfer_addr = (a + 2) & (RAM_BYTES - 1);
        h
    }

    /// DMA4 to the SPU: each word is two halfwords, low first.
    pub fn dma_write(&mut self, words: &[u32]) {
        for &w in words {
            self.transfer_write(w as u16);
            self.transfer_write((w >> 16) as u16);
        }
    }

    /// DMA4 from the SPU.
    pub fn dma_read(&mut self, words: &mut [u32]) {
        for w in words.iter_mut() {
            let lo = self.transfer_read() as u32;
            *w = lo | (self.transfer_read() as u32) << 16;
        }
    }

    /// CD audio input (left, right) for the following ticks.
    pub fn set_cd_input(&mut self, l: i16, r: i16) {
        self.cd_in = [l, r];
    }

    // ---- the sample clock -----------------------------------------------

    fn ram_at(&self, addr: u32) -> u16 {
        self.ram[((addr & (RAM_BYTES - 1)) >> 1) as usize]
    }

    /// The decoder finished a block: apply its loop flags and move to the
    /// next block.
    fn end_of_block(&mut self, vi: usize) {
        let flags = self.voices[vi].flags;
        if flags & 1 != 0 {
            self.endx |= 1 << vi;
            let v = &mut self.voices[vi];
            v.addr = (v.lsa as u32) << 3;
            if flags & 2 == 0 {
                v.phase = Phase::Release;
                v.envx = 0;
            }
        } else {
            let v = &mut self.voices[vi];
            v.addr = (v.addr + 16) & (RAM_BYTES - 1);
        }
    }

    /// Decode the voice's next data halfword (4 samples) into its ring:
    /// the block's header with its first word, the block's loop flags with
    /// its last.
    fn decode_word(&mut self, vi: usize) {
        let k = (self.voices[vi].decoded % 28) as u32;
        let addr = self.voices[vi].addr & (RAM_BYTES - 1) & !1;
        if k == 0 {
            self.touch(addr);
            let hdr = self.ram_at(addr);
            let v = &mut self.voices[vi];
            v.flags = (hdr >> 8) as u8;
            v.shift = (hdr & 0xF) as u8;
            v.filter = ((hdr >> 4) & 0xF) as u8;
            if v.flags & 4 != 0 && !v.lsa_locked {
                v.lsa = (addr >> 3) as u16;
            }
        }
        let waddr = (addr + 2 + 2 * (k / 4)) & (RAM_BYTES - 1);
        self.touch(waddr);
        let word = self.ram_at(waddr);
        let v = &mut self.voices[vi];
        let shift = v.shift as u32;
        let filter = v.filter as usize;
        // oracle: filters 5..15 predict nothing (psx-spx lists 0..4).
        let (f0, f1) = if filter < 5 { (FILTER_POS[filter], FILTER_NEG[filter]) } else { (0, 0) };
        for i in 0..4 {
            let nib = ((word >> (4 * i)) & 0xF) as i32;
            let t = ((nib << 28) >> 28) << 12;
            // oracle: shifts 13..15 decode a negative nibble as -80h and
            // any other as 0 (psx-spx: "reportedly same as for CD-XA",
            // whose reserved shifts act as 9).
            let base = if shift > 12 { if nib & 8 != 0 { -0x80 } else { 0 } } else { t >> shift };
            // oracle: each prediction product is shifted (floored) on its own.
            let s = clamp16(base + ((v.prev[0] * f0) >> 6) + ((v.prev[1] * f1) >> 6));
            v.prev[1] = v.prev[0];
            v.prev[0] = s;
            v.ring[(v.decoded % 32) as usize] = s as i16;
            v.decoded += 1;
        }
        if k + 4 == 28 {
            self.end_of_block(vi);
        }
    }

    fn key_on(&mut self, vi: usize) {
        let v = &mut self.voices[vi];
        v.addr = (v.ssa as u32) << 3;
        v.counter = 0;
        v.decoded = 0;
        v.envx = 0;
        v.env_counter = 0;
        v.phase = Phase::Attack;
        v.prev = [0; 2];
        v.delay = KEY_ON_DELAY;
        v.kon_age = 0;
        self.endx &= !(1 << vi);
    }

    fn adsr(&mut self, vi: usize) {
        let v = &mut self.voices[vi];
        let (a1, a2) = (v.adsr1, v.adsr2);
        // oracle: an attack at the top level moves on to decay on any
        // update, before stepping (even when its own rate never steps).
        if v.phase == Phase::Attack && v.envx == 0x7FFF {
            v.phase = Phase::Decay;
            v.env_counter = 0;
        }
        let rate = match v.phase {
            Phase::Attack => Rate { shift: ((a1 >> 10) & 0x1F) as u8, step: ((a1 >> 8) & 3) as u8, exponential: a1 & 0x8000 != 0, decreasing: false, negative: false },
            Phase::Decay => Rate { shift: ((a1 >> 4) & 0xF) as u8, step: 0, exponential: true, decreasing: true, negative: false },
            Phase::Sustain => Rate { shift: ((a2 >> 8) & 0x1F) as u8, step: ((a2 >> 6) & 3) as u8, exponential: a2 & 0x8000 != 0, decreasing: a2 & 0x4000 != 0, negative: false },
            Phase::Release => Rate { shift: (a2 & 0x1F) as u8, step: 0, exponential: a2 & 0x20 != 0, decreasing: true, negative: false },
        };
        let (level, stepped) = envelope::step(v.envx, &mut v.env_counter, rate);
        v.envx = level;
        // oracle: decay moves on to sustain only on an update that steps, once
        // the level is below the sustain level (psx-spx: "until Sustain
        // Level").
        if stepped && v.phase == Phase::Decay && (v.envx as i32) < ((a1 as i32 & 0xF) + 1) << 11 {
            v.phase = Phase::Sustain;
            v.env_counter = 0;
        }
    }

    /// Sweep or set a volume register's current level.
    fn volume(raw: u16, level: &mut i16, counter: &mut u16) {
        if raw & 0x8000 == 0 {
            *level = (raw << 1) as i16;
            return;
        }
        *level = envelope::sweep(*level, counter, raw);
    }

    /// Test hook for the device oracle: set every volume sweep's step
    /// counter. No register write resets them (only a step does), so a
    /// run's result must not depend on what earlier runs left in them.
    pub fn set_sweep_counters(&mut self, value: u16) {
        for v in self.voices.iter_mut() {
            v.vol_counter = [value; 2];
        }
        self.main_counter = [value; 2];
    }

    fn noise_tick(&mut self) {
        let c = self.cnt();
        let shift = ((c >> 10) & 0xF) as u32;
        let step = 4 + ((c >> 8) & 3) as i32;
        let n = &mut self.noise;
        n.timer -= step;
        let l = n.level;
        let parity = ((l >> 15) ^ (l >> 12) ^ (l >> 11) ^ (l >> 10) ^ 1) & 1;
        if n.timer < 0 {
            n.level = l.wrapping_mul(2) | parity;
            n.timer += 0x20000 >> shift;
            if n.timer < 0 {
                n.timer += 0x20000 >> shift;
            }
        }
    }

    /// One voice's output for this tick, after the envelope.
    fn voice_output(&mut self, vi: usize) -> i32 {
        let noise_on = self.mask(reg::NON) & (1 << vi) != 0;
        // oracle: the decoder runs ahead of the playback position, one data
        // halfword per tick whenever fewer than DECODE_AHEAD samples are
        // buffered (so a block's loop flags take effect ten samples before
        // its end at pitch 1000h).
        let p = self.voices[vi].counter >> 12;
        if self.voices[vi].decoded < p + DECODE_AHEAD {
            self.decode_word(vi);
        }
        while self.voices[vi].decoded < p + 4 {
            self.decode_word(vi);
        }
        let noise = self.noise.level as i16 as i32;
        let v = &mut self.voices[vi];
        let i = ((v.counter >> 4) & 0xFF) as usize;
        let sample = if noise_on {
            noise
        } else {
            // oracle: the counter points at the oldest of the four samples,
            // and the four products are summed before the shift.
            let s = |k: u64| v.ring[((p + k) % 32) as usize] as i32;
            (GAUSS[0xFF - i] * s(0) + GAUSS[0x1FF - i] * s(1) + GAUSS[0x100 + i] * s(2) + GAUSS[i] * s(3)) >> 15
        };
        let out = (sample * v.envx as i32) >> 15;
        v.out = out;
        out
    }

    /// Advance a voice's pitch counter by one tick.
    fn voice_advance(&mut self, vi: usize, pmon_prev: i32) {
        let pmon = vi > 0 && self.mask(reg::PMON) & (1 << vi) != 0;
        let v = &mut self.voices[vi];
        let mut step = v.pitch as u32;
        if pmon {
            let factor = (pmon_prev + 0x8000) as u32;
            step = (((step as i16 as i32) * factor as i32) >> 15) as u32 & 0xFFFF;
        }
        // oracle: steps above 3FFFh clip to 3FFFh (psx-spx's pseudo code
        // says 4000h, its range comment 0..3FFFh).
        if step > 0x3FFF {
            step = 0x3FFF;
        }
        v.counter += step as u64;
    }

    /// Produce one 44.1 kHz sample: the mixed output (left, right).
    pub fn tick(&mut self) -> [i16; 2] {
        let koff = core::mem::take(&mut self.koff);
        let kon = core::mem::take(&mut self.kon);
        for v in self.voices.iter_mut() {
            v.kon_age = v.kon_age.saturating_add(1);
        }
        self.noise_tick();

        let eon = self.mask(reg::EON);
        let (mut l, mut r) = (0i32, 0i32);
        let (mut rl, mut rr) = (0i32, 0i32);
        let mut prev_out = 0;
        let mut v1 = 0;
        let mut v3 = 0;
        for vi in 0..VOICES {
            let out = self.voice_output(vi);
            if vi == 1 {
                v1 = out;
            } else if vi == 3 {
                v3 = out;
            }
            let v = &mut self.voices[vi];
            let vl = (out * v.volx[0] as i32) >> 15;
            let vr = (out * v.volx[1] as i32) >> 15;
            l += vl;
            r += vr;
            if eon & (1 << vi) != 0 {
                rl += vl;
                rr += vr;
            }
            // The envelope and the pitch counter advance after the output,
            // except during the key-on delay.
            if self.voices[vi].delay > 0 {
                let v = &mut self.voices[vi];
                v.delay -= 1;
                // oracle: the end of the key-on hold releases a software
                // repeat address, so loop-start flags apply again.
                if v.delay == 0 {
                    v.lsa_locked = false;
                }
            } else {
                self.adsr(vi);
                self.voice_advance(vi, prev_out);
            }
            prev_out = out;
            let v = &mut self.voices[vi];
            Self::volume(v.vol[0], &mut v.volx[0], &mut v.vol_counter[0]);
            Self::volume(v.vol[1], &mut v.volx[1], &mut v.vol_counter[1]);
        }
        // oracle: key on takes effect after this tick's output and updates
        // (the voice plays its old state once more); key off after the
        // envelope step too, and not at all in the tick after a key on.
        for vi in 0..VOICES {
            if kon & (1 << vi) != 0 {
                self.key_on(vi);
            }
        }
        // oracle: with the SPU disabled, every voice is silenced for good
        // after this tick (level zero, in release; re-enabling does not
        // bring it back).
        if self.cnt() & cnt::ENABLE == 0 {
            for v in self.voices.iter_mut() {
                v.envx = 0;
                v.phase = Phase::Release;
                v.env_counter = 0;
            }
        }
        for vi in 0..VOICES {
            // oracle: a key off on a releasing voice changes nothing (its step
            // counter runs on).
            if koff & (1 << vi) != 0 && self.voices[vi].kon_age > 1 && self.voices[vi].phase != Phase::Release {
                self.voices[vi].phase = Phase::Release;
                self.voices[vi].env_counter = 0;
            }
        }

        // Capture buffers: CD L/R, voice 1, voice 3.
        let idx = self.capture;
        let cd = self.cd_in;
        let data = [cd[0], cd[1], clamp16(v1) as i16, clamp16(v3) as i16];
        for (base, val) in [0x000u32, 0x400, 0x800, 0xC00].into_iter().zip(data) {
            let a = base + 2 * idx;
            self.touch(a);
            self.ram[(a >> 1) as usize] = val as u16;
        }
        self.capture = (idx + 1) & (CAPTURE_SAMPLES - 1);

        let c = self.cnt();
        if c & cnt::CD_ENABLE != 0 {
            let cl = (cd[0] as i32 * self.raw(reg::AVOLL) as i16 as i32) >> 15;
            let cr = (cd[1] as i32 * self.raw(reg::AVOLR) as i16 as i32) >> 15;
            l += cl;
            r += cr;
            if c & cnt::CD_REVERB != 0 {
                rl += cl;
                rr += cr;
            }
        }

        let (wl, wr) = reverb::Reverb::step(self, idx, clamp16(rl), clamp16(rr));
        l += wl;
        r += wr;

        let (ml, mr) = (self.raw(reg::MVOLL), self.raw(reg::MVOLR));
        Self::volume(ml, &mut self.main_volx[0], &mut self.main_counter[0]);
        Self::volume(mr, &mut self.main_volx[1], &mut self.main_counter[1]);
        let mut out = [
            clamp16((clamp16(l) * self.main_volx[0] as i32) >> 15) as i16,
            clamp16((clamp16(r) * self.main_volx[1] as i32) >> 15) as i16,
        ];
        if c & cnt::UNMUTE == 0 || c & cnt::ENABLE == 0 {
            out = [0, 0];
        }
        out
    }
}

#[cfg(test)]
mod tests;
