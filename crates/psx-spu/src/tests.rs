//! Spec-derived unit tests (debug aids; the oracle is the device target).

use super::*;
use alloc::vec::Vec;

/// One ADPCM block from 28 nibbles.
fn block(nibbles: [i8; 28], shift: u8, filter: u8, flags: u8) -> [u16; 8] {
    let mut b = [0u16; 8];
    b[0] = (filter as u16) << 4 | shift as u16 | (flags as u16) << 8;
    for (i, n) in nibbles.iter().enumerate() {
        b[1 + i / 4] |= ((*n as u16) & 0xF) << (4 * (i % 4));
    }
    b
}

fn load(spu: &mut Spu, addr: u32, data: &[u16]) {
    spu.write(reg::TSA, (addr >> 3) as u16);
    for h in data {
        spu.transfer_write(*h);
    }
}

#[test]
fn gauss_table_sums() {
    for i in 0..256 {
        let s = tables::GAUSS[i] + tables::GAUSS[0xFF - i] + tables::GAUSS[0x100 + i] + tables::GAUSS[0x1FF - i];
        assert!((0x7F7F..=0x7F81).contains(&s), "{i}: {s:#x}");
    }
}

#[test]
fn linear_attack_steps_seven_per_tick_at_shift_11() {
    let r = Rate { shift: 11, step: 0, exponential: false, decreasing: false, negative: false };
    let (mut l, mut c) = (0i16, 0u16);
    for n in 1..=10 {
        l = envelope::step(l, &mut c, r).0;
        assert_eq!(l, 7 * n);
    }
    // Shift 13 steps every fourth tick.
    let r = Rate { shift: 13, ..r };
    let (mut l, mut c) = (0i16, 0u16);
    let levels: Vec<i16> = (0..8).map(|_| { l = envelope::step(l, &mut c, r).0; l }).collect();
    assert_eq!(levels, [0, 0, 0, 7, 7, 7, 7, 14]);
}

#[test]
fn never_rate_never_steps() {
    let r = Rate { shift: 0x1F, step: 3, exponential: false, decreasing: true, negative: false };
    let (mut l, mut c) = (0x1234i16, 0u16);
    for _ in 0..100_000 {
        l = envelope::step(l, &mut c, r).0;
    }
    assert_eq!(l, 0x1234);
}

#[test]
fn voice_one_is_captured_and_loops() {
    let mut spu = Spu::new();
    let dc = block([7; 28], 0, 0, 7);
    load(&mut spu, 0x2000, &dc);
    spu.write(reg::SPUCNT, 0xC000);
    for (off, val) in [(reg::PITCH, 0x1000), (reg::SSA, 0x400), (reg::ADSR1, 0x00FF), (reg::ADSR2, 0x1FC0)] {
        spu.write(reg::voice(1, off), val);
    }
    spu.write(reg::KON, 2);
    for _ in 0..100 {
        spu.tick();
    }
    let v = &spu.voices[1];
    // Attack reaches 7FFFh; the decay steps once (-8) before the sustain
    // level check moves to sustain, whose rate never steps.
    assert_eq!(v.envx, 0x7FF7);
    assert_eq!(v.lsa, 0x400);
    assert_eq!(spu.read(reg::ENDX) & 2, 2);
    // DC 7000h through the Gaussian table: 7000h * (sum of the four
    // weights at index 0) >> 15, then the envelope.
    let g = &tables::GAUSS;
    let interp: i32 = ([g[0xFF], g[0x1FF], g[0x100], g[0]].iter().sum::<i32>() * 0x7000) >> 15;
    let want = (interp * 0x7FF7) >> 15;
    let idx = (spu.capture + CAPTURE_SAMPLES - 1) % CAPTURE_SAMPLES;
    assert_eq!(spu.ram()[(0x800 / 2 + idx) as usize] as i16 as i32, want);
}

#[test]
fn transfers_round_trip_and_trigger_irq() {
    let mut spu = Spu::new();
    spu.write(reg::IRQA, 0x3000 >> 3);
    spu.write(reg::SPUCNT, 0x8040);
    load(&mut spu, 0x2FF0, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    assert!(spu.irq());
    assert_ne!(spu.status() & 0x40, 0);
    spu.write(reg::SPUCNT, 0x8000);
    assert!(!spu.irq());
    spu.write(reg::TSA, 0x2FF0 >> 3);
    let mut w = [0u32; 3];
    spu.dma_read(&mut w);
    assert_eq!(w, [0x0002_0001, 0x0004_0003, 0x0006_0005]);
}
