//! Chip spu's cases: libspu where it touches the SPU, and libsnd's
//! `_SsInit` (D59, D63; phase 1 task 9f).
//!
//! Observation notes, beyond the module's specs:
//! - **The device effect** is the SPU's whole state (registers, every
//!   voice's envelope, decoder and counters, the latches, SPU RAM) and
//!   the audio delivered (`Harness::device`), looked at after each step:
//!   the adapters' mixer calls must leave the core where the original's
//!   register writes leave it, tick for tick.
//! - **libspu's and libsnd's state** stays in guest RAM: every step looks
//!   at RAM from 80000000h to the cases' scratch area in 64 KB blocks
//!   (the kernel's events, both libraries' data and bss), so a global an
//!   adapter writes differently shows, wherever it is.
//! - **Time.** The original's loops spend the guest's safepoint budget,
//!   and the budget left is observed after each step (D60, D63); cases
//!   move its phase first with calls to libspu's delay loop
//!   (`_spu_Fw1ts`, 60 passes each) so that safepoints fall inside the
//!   adapted code at seeded places.
//! - Callbacks: the transfer callback (`_spu_transferCallback`, set
//!   through its symbol) is the game's `[conform]` counter.

use super::{Case, Harness, Rng, SCRATCH, call};

/// SPU RAM addresses the cases write samples to (above the capture
/// buffers and `_spu_init`'s block at 1000h).
const SAMPLE_AT: u32 = 0x2000;
/// Guest buffers: sample data, structs, a status buffer.
const DATA: u32 = SCRATCH;
const ATTR: u32 = SCRATCH + 0x4000;
const STATUS: u32 = SCRATCH + 0x4800;

pub fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "spu.init",
            chip: "spu",
            level: 1,
            covers: &["_spu_init", "_spu_FsetRXX"],
            needs: &["ResetCallback", "SpuInit", "_spu_init", "_spu_Fw1ts", "SpuGetVoiceEnvelope"],
            run: init_case,
        },
        Case {
            name: "spu.write",
            chip: "spu",
            level: 1,
            covers: &["_spu_Fw", "_spu_FiDMA", "_spu_FsetRXXa"],
            needs: &[
                "ResetCallback",
                "SpuInit",
                "_spu_Fw1ts",
                "SpuSetTransferMode",
                "SpuSetTransferStartAddr",
                "SpuWritePartly",
                "SpuIsTransferCompleted",
                "_spu_FsetRXXa",
                "_spu_Fw",
            ],
            run: write_case,
        },
        Case {
            name: "spu.registers",
            chip: "spu",
            level: 1,
            covers: &["_spu_FsetRXX"],
            needs: &["ResetCallback", "SpuInit", "_spu_Fw1ts", "_spu_FsetRXX"],
            run: registers_case,
        },
        Case {
            name: "spu.clear-reverb",
            chip: "spu",
            level: 1,
            covers: &["SpuClearReverbWorkArea"],
            needs: &["ResetCallback", "SpuInit", "_spu_Fw1ts", "SpuClearReverbWorkArea", "SpuSetTransferMode"],
            run: clear_reverb_case,
        },
        Case {
            name: "spu.set-key",
            chip: "spu",
            level: 1,
            covers: &["SpuSetKey", "SpuGetVoiceEnvelope"],
            needs: &[
                "ResetCallback",
                "SpuInit",
                "_spu_Fw1ts",
                "SpuSetKey",
                "SpuGetVoiceEnvelope",
                "SpuSetVoiceAttr",
                "SpuSetCommonAttr",
                "SpuSetTransferStartAddr",
                "SpuWritePartly",
                "SpuIsTransferCompleted",
            ],
            run: set_key_case,
        },
        Case {
            name: "spu.key-status",
            chip: "spu",
            level: 1,
            covers: &["SpuGetKeyStatus", "SpuGetAllKeysStatus"],
            needs: &[
                "ResetCallback",
                "SpuInit",
                "_spu_Fw1ts",
                "SpuSetKey",
                "SpuGetKeyStatus",
                "SpuGetAllKeysStatus",
                "SpuSetVoiceAttr",
                "SpuSetTransferStartAddr",
                "SpuWritePartly",
                "SpuIsTransferCompleted",
            ],
            run: key_status_case,
        },
        Case {
            name: "spu.voice-attr",
            chip: "spu",
            level: 1,
            covers: &["SpuSetVoiceAttr"],
            needs: &[
                "ResetCallback",
                "SpuInit",
                "_spu_Fw1ts",
                "SpuSetVoiceAttr",
                "SpuSetKey",
                "SpuSetCommonAttr",
                "SpuSetTransferStartAddr",
                "SpuWritePartly",
                "SpuIsTransferCompleted",
            ],
            run: voice_attr_case,
        },
        Case {
            name: "spu.voice-volume",
            chip: "spu",
            level: 1,
            covers: &["SpuSetVoiceVolume", "SpuGetVoiceVolume"],
            needs: &["ResetCallback", "SpuInit", "_spu_Fw1ts", "SpuSetVoiceVolume", "SpuGetVoiceVolume", "SpuSetVoiceAttr"],
            run: voice_volume_case,
        },
        Case {
            name: "spu.common-attr",
            chip: "spu",
            level: 1,
            covers: &["SpuSetCommonAttr"],
            needs: &["ResetCallback", "SpuInit", "_spu_Fw1ts", "SpuSetCommonAttr"],
            run: common_attr_case,
        },
        Case {
            name: "spu.voice-masks",
            chip: "spu",
            level: 1,
            covers: &["_SpuSetAnyVoice", "_SpuGetAnyVoice", "SpuSetNoiseClock"],
            needs: &[
                "ResetCallback",
                "SpuInit",
                "_spu_Fw1ts",
                "SpuSetNoiseVoice",
                "SpuGetNoiseVoice",
                "SpuSetReverbVoice",
                "SpuGetReverbVoice",
                "SpuSetNoiseClock",
            ],
            run: voice_masks_case,
        },
        Case {
            name: "spu.reverb",
            chip: "spu",
            level: 1,
            covers: &["SpuSetReverb", "SpuSetReverbDepth", "SpuSetReverbModeParam"],
            needs: &["ResetCallback", "SpuInit", "_spu_Fw1ts", "SpuSetReverb", "SpuSetReverbDepth", "SpuSetReverbModeParam", "SpuClearReverbWorkArea"],
            run: reverb_case,
        },
        Case {
            name: "spu.ss-init",
            chip: "spu",
            level: 1,
            covers: &["_SsInit"],
            needs: &["ResetCallback", "SsInit", "_SsInit", "_spu_Fw1ts", "SpuGetVoiceEnvelope"],
            run: ss_init_case,
        },
        Case {
            name: "spu.sound-driver",
            chip: "spu",
            level: 2,
            covers: &[],
            needs: &[
                "ResetCallback",
                "SsInit",
                "_spu_Fw1ts",
                "SpuSetCommonAttr",
                "SpuSetTransferMode",
                "SpuSetTransferStartAddr",
                "SpuWritePartly",
                "SpuIsTransferCompleted",
                "SpuSetVoiceAttr",
                "SpuSetKey",
                "SpuGetVoiceEnvelope",
                "SpuSetReverbModeParam",
                "SpuSetReverbDepth",
                "SpuSetReverb",
                "SpuSetReverbVoice",
                "SpuSetNoiseVoice",
                "SpuSetNoiseClock",
                "SpuClearReverbWorkArea",
            ],
            run: sound_driver,
        },
        Case {
            name: "spu.transfers",
            chip: "spu",
            level: 2,
            covers: &[],
            needs: &[
                "ResetCallback",
                "SpuInit",
                "_spu_Fw1ts",
                "SpuSetTransferMode",
                "SpuSetTransferStartAddr",
                "SpuWritePartly",
                "SpuIsTransferCompleted",
                "SpuClearReverbWorkArea",
                "SpuSetVoiceAttr",
                "SpuSetKey",
            ],
            run: transfers,
        },
    ]
}

// --- Observation ------------------------------------------------------------

/// FNV-1a.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

/// What the libraries' state and the SPU amount to after `what`: the
/// budget left, the device effect (the SPU's state, the audio), RAM below
/// the scratch area in 64 KB blocks (the blocks that differ from zero
/// filled, by index). libetc's interrupt stack is left out, as the
/// harness's stack is: the frames there are each side's own (the
/// original's dispatcher has one above the handlers').
fn look(h: &mut dyn Harness, what: &str) {
    let b = h.budget();
    h.observe(&format!("{what}: budget"), b.to_string());
    let d = h.device();
    h.observe(&format!("{what}: device"), d);
    let stack = h.symbol("ResetCallback").and_then(|f| crate::libetc::intr_stack(&mut |a| u32::from_le_bytes(h.read(a, 4)[..4].try_into().unwrap()), f));
    let mut blocks = Vec::new();
    let mut at = 0x8000_0000u32;
    while at < SCRATCH {
        let mut ram = h.read(at, 0x1_0000);
        for a in stack.iter().flat_map(|s| s.start.max(at)..s.end.min(at + 0x1_0000)) {
            ram[(a - at) as usize] = 0;
        }
        blocks.push(format!("{:x}", fnv(&ram) & 0xFFFF_FFFF));
        at += 0x1_0000;
    }
    h.observe(&format!("{what}: ram"), blocks.join(" "));
}

/// Call and record the result and the budget left.
fn step(h: &mut dyn Harness, name: &str, args: &[u32]) -> Result<u32, String> {
    let v = call(h, name, args)?;
    let b = h.budget();
    h.observe(&format!("{name}: budget"), b.to_string());
    Ok(v)
}

/// Move the safepoint budget's phase: `n` calls of libspu's delay loop.
fn phase(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    let n = rng.range(0, 17);
    for _ in 0..n {
        h.call("_spu_Fw1ts", &[])?;
    }
    let b = h.budget();
    h.observe(&format!("phase {n}: budget"), b.to_string());
    Ok(())
}

fn init(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    call(h, "ResetCallback", &[])?;
    phase(h, rng)?;
    step(h, "SpuInit", &[])?;
    look(h, "init");
    Ok(())
}

fn word(h: &mut dyn Harness, addr: u32) -> u32 {
    let b = h.read(addr, 4);
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn put(h: &mut dyn Harness, addr: u32, v: u32) {
    h.write(addr, &v.to_le_bytes());
}

/// The `[conform]` callback's counter, observed.
fn callbacks(h: &mut dyn Harness, what: &str) {
    if let Some((_, state)) = h.callback() {
        let v = word(h, state);
        h.observe(&format!("{what}: callback state"), format!("{v:#x}"));
    }
}

/// Set libspu's transfer callback to the `[conform]` counter (or clear it).
fn transfer_callback(h: &mut dyn Harness, on: bool) -> Result<(), String> {
    let at = h.symbol("_spu_transferCallback").ok_or("no _spu_transferCallback symbol")?;
    let f = if on { h.callback().ok_or("no [conform] callback")?.0 } else { 0 };
    put(h, at, f);
    Ok(())
}

/// libspu's register-queue mode (`_spu_env` bit 0).
fn queue_mode(h: &mut dyn Harness, on: bool) -> Result<(), String> {
    let at = h.symbol("_spu_env").ok_or("no _spu_env symbol")?;
    put(h, at, on as u32);
    Ok(())
}

// --- Samples and structs ----------------------------------------------------

/// `blocks` ADPCM blocks of seeded nibbles, shifts and filters at `DATA`,
/// the first marking the loop start, the last ending and repeating.
fn sample(h: &mut dyn Harness, rng: &mut Rng, blocks: u32) -> u32 {
    let mut b = Vec::new();
    for i in 0..blocks {
        let shift = rng.range(0, 13) as u8;
        let filter = rng.range(0, 5) as u8;
        let flags = if i == 0 {
            0x04
        } else if i + 1 == blocks {
            0x03
        } else {
            0x00
        };
        b.push(filter << 4 | shift);
        b.push(flags);
        for _ in 0..14 {
            b.push(rng.next() as u8);
        }
    }
    h.write(DATA, &b);
    b.len() as u32
}

/// A sample to SPU RAM at `at` by DMA, waited for.
fn upload(h: &mut dyn Harness, rng: &mut Rng, at: u32, blocks: u32) -> Result<(), String> {
    let len = sample(h, rng, blocks);
    step(h, "SpuSetTransferStartAddr", &[at])?;
    step(h, "SpuWritePartly", &[DATA, len])?;
    step(h, "SpuIsTransferCompleted", &[1])?;
    Ok(())
}

/// SpuCommonAttr: master volume `vol` on both sides, direct.
fn master(h: &mut dyn Harness, vol: u16) -> Result<(), String> {
    let mut a = vec![0u8; 40];
    a[0..4].copy_from_slice(&3u32.to_le_bytes());
    a[4..6].copy_from_slice(&vol.to_le_bytes());
    a[6..8].copy_from_slice(&vol.to_le_bytes());
    h.write(ATTR, &a);
    step(h, "SpuSetCommonAttr", &[ATTR])?;
    Ok(())
}

/// SpuVoiceAttr (64 bytes): the fields by offset.
struct VoiceAttr([u8; 64]);

impl VoiceAttr {
    fn new(voices: u32, mask: u32) -> Self {
        let mut a = VoiceAttr([0; 64]);
        a.w32(0, voices);
        a.w32(4, mask);
        a
    }
    fn w16(&mut self, at: usize, v: u16) {
        self.0[at..at + 2].copy_from_slice(&v.to_le_bytes());
    }
    fn w32(&mut self, at: usize, v: u32) {
        self.0[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn set(&self, h: &mut dyn Harness) -> Result<(), String> {
        h.write(ATTR, &self.0);
        step(h, "SpuSetVoiceAttr", &[ATTR])?;
        Ok(())
    }
}

/// Voices playing the sample at `SAMPLE_AT`: loud, pitch 1000h, a fast
/// attack and full sustain.
fn voices_play(h: &mut dyn Harness, voices: u32, pitch: u16) -> Result<(), String> {
    let mut a = VoiceAttr::new(voices, 0x1 | 0x2 | 0x10 | 0x80 | 0x2_0000 | 0x4_0000);
    a.w16(8, 0x3000);
    a.w16(10, 0x2000);
    a.w16(20, pitch);
    a.w32(28, SAMPLE_AT);
    a.w16(58, 0x00FF);
    a.w16(60, 0x1FC0);
    a.set(h)
}

/// A seeded SpuVoiceAttr: any voices, any mask (sometimes all), fields
/// across their ranges and past them.
fn random_voice_attr(rng: &mut Rng) -> VoiceAttr {
    let voices = match rng.range(0, 4) {
        0 => 0xFF_FFFF,
        1 => 1 << rng.range(0, 24),
        _ => rng.next() as u32 & 0xFF_FFFF,
    };
    let mask = match rng.range(0, 5) {
        0 => 0,
        _ => rng.next() as u32 & 0x7_FFFF,
    };
    let mut a = VoiceAttr::new(voices, mask);
    a.w16(8, rng.next() as u16);
    a.w16(10, rng.next() as u16);
    a.w16(12, rng.range(0, 10) as u16);
    a.w16(14, rng.range(0, 10) as u16);
    a.w16(20, rng.range(0, 0x5000) as u16);
    a.w16(22, (rng.range(0, 128) << 8 | rng.range(0, 128)) as u16);
    a.w16(24, (rng.range(24, 100) << 8 | rng.range(0, 128)) as u16);
    a.w32(28, rng.range(0x1000, 0x7_F000));
    a.w32(32, rng.range(0x1000, 0x7_F000));
    for at in [36, 40, 44] {
        a.w32(at, rng.range(0, 9));
    }
    for at in [48, 50, 52, 54, 56] {
        a.w16(at, rng.range(0, 200) as u16);
    }
    a.w16(58, rng.next() as u16);
    a.w16(60, rng.next() as u16);
    a
}

// --- Level 1 -----------------------------------------------------------------

/// The reset, cold (through `SpuInit`, with `_SpuInit`'s `_spu_FsetRXX`)
/// and hot, with the budget at a seeded phase; then what the voices it
/// keyed on and off do.
fn init_case(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    init(h, rng)?;
    for v in [0, 7, 23] {
        step(h, "SpuGetVoiceEnvelope", &[v, STATUS])?;
    }
    h.vblanks(1)?;
    look(h, "a vblank");
    phase(h, rng)?;
    step(h, "_spu_init", &[rng.range(0, 2)])?;
    look(h, "_spu_init");
    h.vblanks(2)?;
    look(h, "two vblanks");
    Ok(())
}

/// SPU RAM writes by DMA or through the FIFO (`SpuSetTransferMode`), at a
/// seeded address and length, with and without the transfer callback;
/// the address helper on its own.
fn write_case(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    init(h, rng)?;
    let fifo = rng.range(0, 2);
    let cb = rng.range(0, 2) == 1;
    transfer_callback(h, cb)?;
    step(h, "SpuSetTransferMode", &[fifo])?;
    let at = rng.range(0x1010, 0x7_E000);
    step(h, "SpuSetTransferStartAddr", &[at])?;
    let blocks = rng.range(1, if fifo == 1 { 24 } else { 200 });
    let len = sample(h, rng, blocks) - rng.range(0, 8);
    phase(h, rng)?;
    step(h, "SpuWritePartly", &[DATA, len])?;
    step(h, "SpuIsTransferCompleted", &[0])?;
    callbacks(h, "after the write");
    look(h, "SpuWritePartly");
    h.vblanks(1)?;
    step(h, "SpuIsTransferCompleted", &[0])?;
    step(h, "SpuIsTransferCompleted", &[1])?;
    look(h, "a vblank");
    for _ in 0..4 {
        let idx = match rng.range(0, 5) {
            0 => u32::MAX,
            1 => u32::MAX - 1,
            2 => rng.range(0, 24) * 8 + 3,
            3 => rng.range(0, 24) * 8 + 7,
            _ => 0xD3,
        };
        step(h, "_spu_FsetRXXa", &[idx, rng.range(0, 0x8_0000)])?;
    }
    step(h, "SpuSetTransferStartAddr", &[rng.range(0x1010, 0x7_E000)])?;
    step(h, "_spu_Fw", &[DATA, rng.range(1, 0x200)])?;
    h.vblanks(1)?;
    callbacks(h, "after _spu_Fw");
    look(h, "_spu_Fw");
    Ok(())
}

/// Registers set by index, raw and as SPU RAM addresses.
fn registers_case(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    init(h, rng)?;
    for _ in 0..12 {
        let idx = match rng.range(0, 6) {
            0 | 1 => rng.range(0, 0xC0),
            2 => [0xC0, 0xC1, 0xC2, 0xC3, 0xD8, 0xD9, 0xDA, 0xDB][rng.range(0, 8) as usize],
            3 => [0xC8, 0xC9, 0xCA, 0xCB, 0xCC, 0xCD][rng.range(0, 6) as usize],
            4 => [0xD1, 0xD2, 0xD3, 0xD6][rng.range(0, 4) as usize],
            _ => rng.range(0xE0, 0x100),
        };
        step(h, "_spu_FsetRXX", &[idx, rng.next() as u32 & 0x7_FFFF, rng.range(0, 2)])?;
    }
    look(h, "registers");
    h.vblanks(1)?;
    look(h, "a vblank");
    Ok(())
}

/// A reverb mode's work area zeroed (modes past 9 refused), with the
/// FIFO mode and a transfer callback set aside meanwhile.
fn clear_reverb_case(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    init(h, rng)?;
    let fifo = rng.range(0, 2);
    step(h, "SpuSetTransferMode", &[fifo])?;
    transfer_callback(h, rng.range(0, 2) == 1)?;
    phase(h, rng)?;
    let mode = rng.range(0, 12);
    step(h, "SpuClearReverbWorkArea", &[mode])?;
    callbacks(h, "after the clear");
    look(h, "SpuClearReverbWorkArea");
    Ok(())
}

/// Keys on and off, valid and not, between vblanks, with the envelopes
/// read back; in queue mode too.
fn set_key_case(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    init(h, rng)?;
    master(h, 0x3FFF)?;
    upload(h, rng, SAMPLE_AT, 8)?;
    let voices = rng.next() as u32 & 0xFF_FFFF | 1;
    voices_play(h, voices, 0x1000)?;
    let queued = rng.range(0, 4) == 0;
    queue_mode(h, queued)?;
    for i in 0..8 {
        let op = [0, 1, 1, 0, 2][rng.range(0, 5) as usize];
        let bits = if rng.range(0, 3) == 0 { voices } else { rng.next() as u32 };
        phase(h, rng)?;
        step(h, "SpuSetKey", &[op, bits])?;
        if rng.range(0, 2) == 0 {
            h.vblanks(rng.range(1, 4))?;
        }
        step(h, "SpuGetVoiceEnvelope", &[rng.range(0, 24), STATUS])?;
        super::observe_ram(h, "envelope", STATUS, 2);
        if i % 3 == 2 {
            look(h, &format!("keys {i}"));
        }
    }
    h.vblanks(2)?;
    look(h, "keys");
    Ok(())
}

/// Key status, one voice and all, as voices sound, release and stop.
fn key_status_case(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    init(h, rng)?;
    upload(h, rng, SAMPLE_AT, 4)?;
    let voices = rng.next() as u32 & 0xFF_FFFF;
    voices_play(h, voices, 0x800)?;
    for _ in 0..6 {
        step(h, "SpuSetKey", &[rng.range(0, 2), rng.next() as u32])?;
        h.vblanks(rng.range(0, 3))?;
        let bit = match rng.range(0, 4) {
            0 => 0,
            1 => rng.next() as u32,
            _ => 1 << rng.range(0, 24),
        };
        phase(h, rng)?;
        step(h, "SpuGetKeyStatus", &[bit])?;
        step(h, "SpuGetAllKeysStatus", &[STATUS])?;
        super::observe_ram(h, "statuses", STATUS, 24);
    }
    look(h, "key status");
    Ok(())
}

/// Seeded voice attributes, then the voices keyed on and heard.
fn voice_attr_case(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    init(h, rng)?;
    master(h, 0x3FFF)?;
    upload(h, rng, SAMPLE_AT, 16)?;
    for i in 0..3 {
        let a = random_voice_attr(rng);
        phase(h, rng)?;
        a.set(h)?;
        look(h, &format!("SpuSetVoiceAttr {i}"));
    }
    voices_play(h, 1 << rng.range(0, 24), 0x1000)?;
    step(h, "SpuSetKey", &[1, 0xFF_FFFF])?;
    h.vblanks(3)?;
    look(h, "keyed on");
    Ok(())
}

/// Voice volumes set and read back, levels and sweeps.
fn voice_volume_case(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    init(h, rng)?;
    for _ in 0..6 {
        let v = rng.range(0, 24);
        if rng.range(0, 2) == 0 {
            phase(h, rng)?;
            step(h, "SpuSetVoiceVolume", &[v, rng.next() as u32, rng.next() as u32])?;
        } else {
            let mut a = VoiceAttr::new(1 << v, 0xF);
            a.w16(8, rng.next() as u16);
            a.w16(10, rng.next() as u16);
            a.w16(12, rng.range(0, 8) as u16);
            a.w16(14, rng.range(0, 8) as u16);
            a.set(h)?;
        }
        step(h, "SpuGetVoiceVolume", &[v, STATUS, STATUS + 2])?;
        super::observe_ram(h, "volumes", STATUS, 4);
    }
    look(h, "volumes");
    Ok(())
}

/// Seeded common attributes: master volumes and modes, CD and external
/// inputs.
fn common_attr_case(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    init(h, rng)?;
    for i in 0..3 {
        let mut a = vec![0u8; 40];
        let mask = if rng.range(0, 4) == 0 { 0 } else { rng.next() as u32 & 0x3FFF };
        a[0..4].copy_from_slice(&mask.to_le_bytes());
        for at in [4usize, 6, 16, 18, 28, 30] {
            a[at..at + 2].copy_from_slice(&(rng.next() as u16).to_le_bytes());
        }
        for at in [8usize, 10] {
            a[at..at + 2].copy_from_slice(&(rng.range(0, 10) as u16).to_le_bytes());
        }
        for at in [20usize, 24, 32, 36] {
            a[at..at + 4].copy_from_slice(&rng.range(0, 2).to_le_bytes());
        }
        h.write(ATTR, &a);
        step(h, "SpuSetCommonAttr", &[ATTR])?;
        look(h, &format!("SpuSetCommonAttr {i}"));
    }
    h.vblanks(1)?;
    look(h, "a vblank");
    Ok(())
}

/// The noise and reverb voice masks (on, off, set, and an unknown op) and
/// the noise clock; in queue mode too.
fn voice_masks_case(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    init(h, rng)?;
    queue_mode(h, rng.range(0, 4) == 0)?;
    for _ in 0..6 {
        let op = [0, 1, 8, 3][rng.range(0, 4) as usize];
        let (set, get) = if rng.range(0, 2) == 0 { ("SpuSetNoiseVoice", "SpuGetNoiseVoice") } else { ("SpuSetReverbVoice", "SpuGetReverbVoice") };
        step(h, set, &[op, rng.next() as u32])?;
        step(h, get, &[])?;
        step(h, "SpuSetNoiseClock", &[(rng.range(0, 90) as i32 - 10) as u32])?;
    }
    look(h, "masks");
    Ok(())
}

/// Reverb modes (some refused, some clearing their area), delay time and
/// feedback, depth, on and off, and a voice heard through it.
fn reverb_case(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    init(h, rng)?;
    for i in 0..3 {
        let mut a = vec![0u8; 20];
        let mask = if rng.range(0, 3) == 0 { 0 } else { rng.next() as u32 & 0x1F };
        let mode = rng.range(0, 11) | if rng.range(0, 3) == 0 { 0x100 } else { 0 };
        a[0..4].copy_from_slice(&mask.to_le_bytes());
        a[4..8].copy_from_slice(&mode.to_le_bytes());
        a[8..10].copy_from_slice(&(rng.next() as u16 & 0x7FFF).to_le_bytes());
        a[10..12].copy_from_slice(&(rng.next() as u16 & 0x7FFF).to_le_bytes());
        a[12..16].copy_from_slice(&rng.range(0, 128).to_le_bytes());
        a[16..20].copy_from_slice(&rng.range(0, 128).to_le_bytes());
        h.write(ATTR, &a);
        phase(h, rng)?;
        step(h, "SpuSetReverbModeParam", &[ATTR])?;
        look(h, &format!("SpuSetReverbModeParam {i}"));
        let mut d = vec![0u8; 20];
        d[0..4].copy_from_slice(&[0u32, 2, 4, 6][rng.range(0, 4) as usize].to_le_bytes());
        d[8..10].copy_from_slice(&(rng.next() as u16 & 0x7FFF).to_le_bytes());
        d[10..12].copy_from_slice(&(rng.next() as u16 & 0x7FFF).to_le_bytes());
        h.write(ATTR + 0x100, &d);
        step(h, "SpuSetReverbDepth", &[ATTR + 0x100])?;
        step(h, "SpuSetReverb", &[rng.range(0, 3)])?;
    }
    h.vblanks(2)?;
    look(h, "reverb");
    Ok(())
}

/// libsnd's init (`SsInit`: libspu's init, the reverb area cleared, then
/// `_SsInit` and the voice manager), and `_SsInit` again.
fn ss_init_case(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    call(h, "ResetCallback", &[])?;
    phase(h, rng)?;
    step(h, "SsInit", &[])?;
    look(h, "SsInit");
    for v in [0, 12, 23] {
        step(h, "SpuGetVoiceEnvelope", &[v, STATUS])?;
    }
    h.vblanks(rng.range(0, 3))?;
    phase(h, rng)?;
    step(h, "_SsInit", &[])?;
    look(h, "_SsInit");
    h.vblanks(1)?;
    look(h, "a vblank");
    Ok(())
}

// --- Level 2 -----------------------------------------------------------------

/// What a sound driver does over libsnd and libspu: init through libsnd,
/// master volume, samples up by DMA and through the FIFO, a reverb mode
/// with its depth, voices by note through the reverb and the noise, keys
/// on, heard for half a second, released, the reverb area cleared.
fn sound_driver(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    call(h, "ResetCallback", &[])?;
    step(h, "SsInit", &[])?;
    look(h, "SsInit");
    master(h, 0x3FFF)?;
    step(h, "SpuSetTransferMode", &[0])?;
    upload(h, rng, SAMPLE_AT, 32)?;
    step(h, "SpuSetTransferMode", &[1])?;
    upload(h, rng, SAMPLE_AT + 0x400, 6)?;
    step(h, "SpuSetTransferMode", &[0])?;
    look(h, "samples");
    let mut r = vec![0u8; 20];
    r[0..4].copy_from_slice(&0x1Fu32.to_le_bytes());
    r[4..8].copy_from_slice(&(3 | 0x100u32).to_le_bytes());
    r[8..10].copy_from_slice(&0x2000u16.to_le_bytes());
    r[10..12].copy_from_slice(&0x2000u16.to_le_bytes());
    h.write(ATTR, &r);
    step(h, "SpuSetReverbModeParam", &[ATTR])?;
    step(h, "SpuSetReverbDepth", &[ATTR])?;
    step(h, "SpuSetReverb", &[1])?;
    look(h, "reverb");
    // Six voices by note, two of them into the reverb, one noise.
    for v in 0..6u32 {
        let mut a = VoiceAttr::new(1 << v, 0x1 | 0x2 | 0x20 | 0x40 | 0x80 | 0x800 | 0x1000 | 0x2000 | 0x4000 | 0x8000 | 0x1_0000);
        a.w16(8, 0x1800);
        a.w16(10, 0x1800);
        a.w16(22, ((48 + 5 * v) << 8) as u16);
        a.w16(24, 60 << 8);
        a.w32(28, if v % 2 == 0 { SAMPLE_AT } else { SAMPLE_AT + 0x400 });
        a.w32(32, if v % 2 == 0 { SAMPLE_AT } else { SAMPLE_AT + 0x400 });
        a.w16(48, 0x10);
        a.w16(50, 0x8);
        a.w16(52, 0x7F);
        a.w16(54, 0x10);
        a.w16(56, 0xC);
        a.set(h)?;
    }
    step(h, "SpuSetReverbVoice", &[1, 0b11])?;
    step(h, "SpuSetNoiseClock", &[0x20])?;
    step(h, "SpuSetNoiseVoice", &[1, 0b10_0000])?;
    step(h, "SpuSetKey", &[1, 0x3F])?;
    h.vblanks(30)?;
    for v in 0..6 {
        step(h, "SpuGetVoiceEnvelope", &[v, STATUS])?;
    }
    look(h, "playing");
    step(h, "SpuSetKey", &[0, 0x15])?;
    h.vblanks(10)?;
    look(h, "released");
    step(h, "SpuSetReverb", &[0])?;
    step(h, "SpuClearReverbWorkArea", &[3])?;
    h.vblanks(2)?;
    look(h, "cleared");
    Ok(())
}

/// Transfers back to back, by DMA with the callback and without, polled
/// across vblanks, a reverb area cleared in between, and a voice playing
/// over the uploads.
fn transfers(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    init(h, rng)?;
    master(h, 0x3FFF)?;
    upload(h, rng, SAMPLE_AT, 4)?;
    voices_play(h, 1, 0x1000)?;
    step(h, "SpuSetKey", &[1, 1])?;
    for (i, (fifo, cb, blocks)) in [(0, true, 150), (0, false, 40), (1, false, 10), (0, true, 3)].into_iter().enumerate() {
        step(h, "SpuSetTransferMode", &[fifo])?;
        transfer_callback(h, cb)?;
        let len = sample(h, rng, blocks);
        step(h, "SpuSetTransferStartAddr", &[0x8000 + 0x4000 * i as u32])?;
        step(h, "SpuWritePartly", &[DATA, len])?;
        for _ in 0..3 {
            step(h, "SpuIsTransferCompleted", &[0])?;
            h.vblanks(1)?;
        }
        callbacks(h, &format!("transfer {i}"));
        look(h, &format!("transfer {i}"));
        if i == 1 {
            step(h, "SpuClearReverbWorkArea", &[1])?;
            look(h, "cleared");
        }
    }
    transfer_callback(h, false)?;
    h.vblanks(2)?;
    look(h, "transfers");
    Ok(())
}
