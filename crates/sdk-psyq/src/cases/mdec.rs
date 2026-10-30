//! Chip mdec's cases: libpress's MDEC functions (D59, D64; phase 1 task
//! 9e).
//!
//! Observation notes, beyond the module's specs:
//! - **The device effect** is the decoded pixels: the output buffer in
//!   RAM (filled with a pattern first, so words DMA1 didn't write show),
//!   hashed, with its first words; and the command word `DecDCTin`
//!   rewrites.
//! - **libpress's state** through the functions that stay original
//!   (`DecDCTGetEnv`: its tables) and the sync calls' polling modes (the
//!   MDEC taking input, DMA1 running).
//! - **Callbacks** (`DecDCTinCallback`, `DecDCToutCallback`: libetc's DMA
//!   callbacks for channels 0 and 1) by the `[conform]` callback's count,
//!   after each call: an interrupt a call raises is taken as it returns to
//!   the host, on both sides.
//! - **Timeouts.** An input that stops short of its command's count leaves
//!   the MDEC taking input, and DMA1 waiting for it; libpress's waits then
//!   spin to their timeout, about 0x100000 loop iterations, and print.
//!   Cases let a vblank pass first and observe Vcount after.
//! - **Return values** of the void functions too: what libpress leaves in
//!   v0 (a DMA register's address, the reset's control word, `printf`'s
//!   result).
//! - Inputs are run-length codes as `DecDCTvlc` makes them, random, in
//!   colour (24- or 15-bit by `DecDCTin`'s mode) and monochrome (8- or
//!   4-bit), sometimes signed, with command counts that match what's
//!   written, fall short of it (the rest parsed as commands) or run past it.

use super::{Case, Harness, Rng, SCRATCH, call, observe_ram};

pub fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "mdec.reset",
            chip: "mdec",
            level: 1,
            covers: &["DecDCTReset"],
            needs: &["ResetCallback", "VSync", "DecDCTReset", "DecDCTin", "DecDCTout", "DecDCTinSync", "DecDCToutSync", "DecDCTGetEnv", "DecDCToutCallback"],
            run: reset,
        },
        Case {
            name: "mdec.decode",
            chip: "mdec",
            level: 1,
            covers: &["DecDCTin", "DecDCTout"],
            needs: &["ResetCallback", "DecDCTReset", "DecDCTin", "DecDCTout", "DecDCTinSync", "DecDCToutSync", "DecDCTinCallback", "DecDCToutCallback"],
            run: decode,
        },
        Case {
            name: "mdec.put-env",
            chip: "mdec",
            level: 1,
            covers: &["DecDCTPutEnv"],
            needs: &["ResetCallback", "VSync", "DecDCTReset", "DecDCTPutEnv", "DecDCTGetEnv", "DecDCTin", "DecDCTout", "DecDCTinSync", "DecDCTinCallback"],
            run: put_env,
        },
        Case {
            name: "mdec.sync",
            chip: "mdec",
            level: 1,
            covers: &["DecDCTinSync", "DecDCToutSync"],
            needs: &["ResetCallback", "VSync", "DecDCTReset", "DecDCTin", "DecDCTout", "DecDCTinSync", "DecDCToutSync", "DecDCToutCallback"],
            run: sync,
        },
        Case {
            name: "mdec.sequence",
            chip: "mdec",
            level: 2,
            covers: &["DecDCTReset", "DecDCTPutEnv", "DecDCTin", "DecDCTout", "DecDCTinSync", "DecDCToutSync"],
            needs: &[
                "ResetCallback", "VSync", "DecDCTReset", "DecDCTPutEnv", "DecDCTGetEnv", "DecDCTin", "DecDCTout", "DecDCTinSync",
                "DecDCToutSync", "DecDCTinCallback", "DecDCToutCallback",
            ],
            run: sequence,
        },
    ]
}

// Scratch layout: two input buffers, the output, two environments.
const IN: u32 = SCRATCH;
const IN2: u32 = SCRATCH + 0x2000;
const OUT: u32 = SCRATCH + 0x4000;
const OUT_LEN: u32 = 0x6000;
const ENV: u32 = SCRATCH + 0xA000;
const ENV2: u32 = SCRATCH + 0xA100;
/// A DECDCTENV: luma and chroma quantization (64 bytes each), the scale
/// table (64 halfwords).
const ENV_LEN: usize = 256;

fn word(h: &mut dyn Harness, addr: u32) -> u32 {
    let b = h.read(addr, 4);
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn observe_word(h: &mut dyn Harness, what: &str, addr: u32) {
    let v = word(h, addr);
    h.observe(what, format!("{v:#010x}"));
}

fn callback(h: &mut dyn Harness) -> Result<(u32, u32), String> {
    h.callback().ok_or_else(|| "game.toml [conform] has no callback".to_string())
}

/// The output buffer: its first words and an FNV-1a hash of all of it.
fn observe_out(h: &mut dyn Harness, what: &str) {
    let b = h.read(OUT, OUT_LEN as usize);
    let hash = b.iter().fold(0xcbf2_9ce4_8422_2325u64, |x, &v| (x ^ v as u64).wrapping_mul(0x100_0000_01b3));
    let first: String = b[..32].iter().map(|x| format!("{x:02x}")).collect();
    h.observe(what, format!("{first}… fnv {hash:016x}"));
}

fn fill_out(h: &mut dyn Harness, rng: &mut Rng) {
    let v = rng.range(0, 256) as u8 | 1;
    h.write(OUT, &vec![v; OUT_LEN as usize]);
}

/// Run-length codes for `n` macroblocks, as `DecDCTvlc` makes them: per
/// block a DC term (quantizer scale, 10-bit value), AC terms (a run of
/// zeros, a 10-bit level), an end code; six blocks a macroblock in
/// colour (Cr, Cb, Y1-Y4), one in monochrome.
fn codes(rng: &mut Rng, n: u32, color: bool) -> Vec<u16> {
    let mut h = Vec::new();
    for _ in 0..n {
        let q = rng.range(1, 64) as u16;
        for _ in 0..if color { 6 } else { 1 } {
            h.push(q << 10 | rng.range(0, 0x400) as u16);
            let mut k = 0;
            for _ in 0..rng.range(0, 10) {
                let run = [0, 0, 1, 2, 3, 7][rng.range(0, 6) as usize];
                if k + run + 1 > 63 {
                    break;
                }
                k += run + 1;
                h.push((run as u16) << 10 | rng.range(1, 0x400) as u16);
            }
            h.push(0xFE00);
        }
    }
    h
}

/// How a stream's command count relates to what's written.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Count {
    /// Padded to whole 32-word blocks, the count exact.
    Whole,
    /// Unpadded under 32 words: DMA0's block carries what follows (end
    /// codes, which aren't commands).
    Short,
    /// Past what's written: the MDEC keeps taking input.
    Over,
}

/// A decode command and `n` macroblocks' codes at `at`; the decoded words
/// per macroblock by `DecDCTin`'s `mode`.
fn put_stream(h: &mut dyn Harness, rng: &mut Rng, at: u32, n: u32, color: bool, count: Count) -> u32 {
    let mut c = codes(rng, n, color);
    let words = (c.len() as u32).div_ceil(2);
    let padded = if count == Count::Short && words < 32 { words } else { words.div_ceil(32) * 32 };
    c.resize(2 * (padded.max(32) as usize), 0xFE00);
    let n_words = match count {
        Count::Over => padded + rng.range(1, 32),
        _ => padded,
    };
    let signed = rng.range(0, 6) == 0;
    let cmd = 0x2000_0000 | (color as u32) << 28 | (signed as u32) << 26 | n_words;
    let mut b = cmd.to_le_bytes().to_vec();
    b.extend(c.iter().flat_map(|v| v.to_le_bytes()));
    h.write(at, &b);
    n_words
}

/// Words a macroblock decodes to.
fn per_macroblock(color: bool, mode: u32) -> u32 {
    match (color, mode & 1 != 0) {
        (true, true) => 192,
        (true, false) => 128,
        (false, false) => 16,
        (false, true) => 8,
    }
}

/// `DecDCTin`, and the command word it rewrote.
fn dct_in(h: &mut dyn Harness, at: u32, mode: u32) -> Result<(), String> {
    call(h, "DecDCTin", &[at, mode])?;
    observe_word(h, "command word", at);
    Ok(())
}

/// The MDEC's and DMA1's busy bits, and the callbacks so far.
fn state(h: &mut dyn Harness, what: &str, cb_state: u32) -> Result<(), String> {
    let i = h.call("DecDCTinSync", &[1])?;
    let o = h.call("DecDCToutSync", &[1])?;
    h.observe(&format!("{what}: taking input, DMA1 running"), format!("{i} {o}"));
    observe_word(h, &format!("{what}: callbacks"), cb_state);
    Ok(())
}

fn vcount(h: &mut dyn Harness, what: &str) -> Result<u32, String> {
    let v = h.call("VSync", &[u32::MAX])?;
    h.observe(&format!("{what}: Vcount"), v.to_string());
    Ok(v)
}

/// A random DECDCTENV at `at`: quantization 1-255 (sometimes 0), the scale
/// table random.
fn put_env_tables(h: &mut dyn Harness, rng: &mut Rng, at: u32) {
    let mut b: Vec<u8> = (0..128).map(|_| if rng.range(0, 20) == 0 { 0 } else { rng.range(1, 256) as u8 }).collect();
    b.extend((0..128).map(|_| rng.range(0, 256) as u8));
    h.write(at, &b);
}

fn reset(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    let (cb, st) = callback(h)?;
    h.call("ResetCallback", &[])?;
    call(h, "DecDCToutCallback", &[cb])?;
    fill_out(h, rng);
    for i in 0..4 {
        // What the reset finds: nothing; a decode taking input; DMA1
        // waiting for it.
        let busy = rng.range(0, 3);
        if busy > 0 {
            let n = rng.range(1, 4);
            put_stream(h, rng, IN, n, true, Count::Over);
            dct_in(h, IN, rng.range(0, 4))?;
            if busy == 2 {
                call(h, "DecDCTout", &[OUT, rng.range(1, 8) * 192])?;
            }
        }
        state(h, &format!("before reset {i}"), st)?;
        let mode = match rng.range(0, 6) {
            0 | 1 => 0,
            2 | 3 => 1,
            4 => rng.range(2, 0x100),
            _ => 0x8000_0000 | rng.range(0, 0x1000),
        };
        call(h, "DecDCTReset", &[mode])?;
        state(h, &format!("after reset {i}"), st)?;
        observe_out(h, &format!("output after reset {i}"));
        call(h, "DecDCTGetEnv", &[ENV])?;
        observe_ram(h, &format!("environment after reset {i}"), ENV, ENV_LEN);
    }
    Ok(())
}

fn decode(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    let (cb, st) = callback(h)?;
    h.call("ResetCallback", &[])?;
    call(h, "DecDCTReset", &[0])?;
    if rng.range(0, 3) != 0 {
        call(h, "DecDCToutCallback", &[cb])?;
    }
    if rng.range(0, 3) == 0 {
        call(h, "DecDCTinCallback", &[cb])?;
    }
    fill_out(h, rng);
    let mut at = 0u32;
    for f in 0..3 {
        let color = rng.range(0, 4) != 0;
        let mode = rng.range(0, 4);
        let n = rng.range(1, 7);
        let count = if rng.range(0, 3) == 0 { Count::Short } else { Count::Whole };
        let input = if f % 2 == 0 { IN } else { IN2 };
        put_stream(h, rng, input, n, color, count);
        let total = n * per_macroblock(color, mode);
        // Sometimes DMA1 first: with the MDEC idle it reads what is there
        // (zeros past the output), as the device does.
        let early = rng.range(0, 3) == 0;
        let slice = |rng: &mut Rng| [8, 16, 32, 40, 64, 128, 192, 200, 384][rng.range(0, 9) as usize];
        if early {
            let s = slice(rng);
            call(h, "DecDCTout", &[OUT + 4 * at, s])?;
            state(h, &format!("frame {f}: out first"), st)?;
            at += 32 * (s >> 5).max(1);
        }
        dct_in(h, input, mode)?;
        state(h, &format!("frame {f}: in"), st)?;
        let mut left = total as i64 - if early { at as i64 } else { 0 };
        let mut k = 0;
        while left > -64 && at < OUT_LEN / 4 - 512 && k < 12 {
            let s = slice(rng);
            call(h, "DecDCTout", &[OUT + 4 * at, s])?;
            state(h, &format!("frame {f}: out {k}"), st)?;
            let w = 32 * (s >> 5).max(1);
            at += w;
            left -= w as i64;
            k += 1;
        }
        h.vblanks(1)?;
        observe_word(h, &format!("frame {f}: callbacks after a vblank"), st);
        observe_out(h, &format!("frame {f}: output"));
    }
    Ok(())
}

fn put_env(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    let (cb, st) = callback(h)?;
    h.call("ResetCallback", &[])?;
    call(h, "DecDCTReset", &[0])?;
    call(h, "DecDCTinCallback", &[cb])?;
    fill_out(h, rng);
    for i in 0..2 {
        put_env_tables(h, rng, ENV);
        // Once in a while into a decode still taking input: the load waits
        // for it, to libpress's timeout.
        let wait = rng.range(0, 3) == 0;
        if wait {
            put_stream(h, rng, IN, 1, true, Count::Over);
            dct_in(h, IN, 1)?;
            h.vblanks(1)?;
            vcount(h, &format!("env {i}: before the wait"))?;
        }
        call(h, "DecDCTPutEnv", &[ENV])?;
        if wait {
            vcount(h, &format!("env {i}: after the wait"))?;
        }
        state(h, &format!("env {i}"), st)?;
        call(h, "DecDCTGetEnv", &[ENV2])?;
        observe_ram(h, &format!("env {i}: what libpress keeps"), ENV2, ENV_LEN);
        let color = rng.range(0, 3) != 0;
        let mode = rng.range(0, 4);
        let n = rng.range(1, 5);
        put_stream(h, rng, IN, n, color, Count::Whole);
        dct_in(h, IN, mode)?;
        call(h, "DecDCTout", &[OUT, n * per_macroblock(color, mode)])?;
        state(h, &format!("env {i}: decoded"), st)?;
        observe_out(h, &format!("env {i}: output"));
    }
    Ok(())
}

fn sync(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    let (cb, st) = callback(h)?;
    h.call("ResetCallback", &[])?;
    call(h, "DecDCTReset", &[0])?;
    call(h, "DecDCToutCallback", &[cb])?;
    fill_out(h, rng);
    // Idle: nothing to wait for.
    for mode in [0, 1, rng.range(2, 0x100)] {
        call(h, "DecDCTinSync", &[mode])?;
        call(h, "DecDCToutSync", &[mode])?;
    }
    // A whole decode, DMA1 for part of it, then the rest.
    let n = rng.range(1, 5);
    put_stream(h, rng, IN, n, true, Count::Whole);
    dct_in(h, IN, 1)?;
    call(h, "DecDCTinSync", &[0])?;
    call(h, "DecDCTout", &[OUT, 192])?;
    call(h, "DecDCToutSync", &[0])?;
    state(h, "decoded", st)?;
    // Taking input: the polling modes say so, and a wait times out
    // (DecDCTinSync or DecDCToutSync, with DMA1 waiting), prints, and
    // resets the MDEC; DMA1 takes zeros.
    let which = if rng.range(0, 2) == 0 { "DecDCTinSync" } else { "DecDCToutSync" };
    let (n, color) = (rng.range(1, 3), rng.range(0, 3) != 0);
    put_stream(h, rng, IN2, n, color, Count::Over);
    dct_in(h, IN2, rng.range(0, 4))?;
    call(h, "DecDCTout", &[OUT + 0x1000, rng.range(4, 12) * 64])?;
    state(h, "short", st)?;
    h.vblanks(1)?;
    vcount(h, "before the wait")?;
    call(h, which, &[0])?;
    vcount(h, "after the wait")?;
    state(h, "after the timeout", st)?;
    observe_out(h, "output after the timeout");
    // Tables again (the reset after a timeout doesn't load them), and a
    // decode the waits see through.
    call(h, "DecDCTReset", &[0])?;
    put_stream(h, rng, IN, 1, false, Count::Whole);
    dct_in(h, IN, rng.range(0, 4))?;
    call(h, "DecDCTout", &[OUT, 16])?;
    call(h, "DecDCTinSync", &[0])?;
    call(h, "DecDCToutSync", &[0])?;
    state(h, "end", st)?;
    observe_out(h, "output at the end");
    Ok(())
}

/// A movie player's calls: init, frames decoded slice by slice under an
/// out callback, DMA1 before its input, output left over into the next
/// frame, new tables mid-stream, a reset with DMA1 waiting, a short input
/// waited out to the timeout, bad options, the callbacks taken off.
fn sequence(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    let (cb, st) = callback(h)?;
    h.call("ResetCallback", &[])?;
    call(h, "DecDCTReset", &[0])?;
    call(h, "DecDCToutCallback", &[cb])?;
    call(h, "DecDCTinCallback", &[cb])?;
    call(h, "DecDCTGetEnv", &[ENV2])?;
    observe_ram(h, "libpress's tables", ENV2, ENV_LEN);
    fill_out(h, rng);
    // Frames of four macroblocks, 24-bit, one slice each.
    for f in 0..3u32 {
        let input = if f % 2 == 0 { IN } else { IN2 };
        put_stream(h, rng, input, 4, true, Count::Whole);
        if f == 2 {
            call(h, "DecDCTout", &[OUT, 192])?;
            state(h, "out before in", st)?;
        }
        dct_in(h, input, if f == 1 { 3 } else { 1 })?;
        for s in (f == 2) as u32..4 {
            call(h, "DecDCTout", &[OUT + 768 * s, 192])?;
        }
        state(h, &format!("frame {f}"), st)?;
        h.vblanks(1)?;
        observe_word(h, &format!("frame {f}: callbacks"), st);
        observe_out(h, &format!("frame {f}: output"));
    }
    // Half a frame taken, then the next frame's input behind what's left.
    put_stream(h, rng, IN, 4, true, Count::Whole);
    dct_in(h, IN, 1)?;
    call(h, "DecDCTout", &[OUT, 384])?;
    put_stream(h, rng, IN2, 2, true, Count::Whole);
    dct_in(h, IN2, 1)?;
    for s in 0..4u32 {
        call(h, "DecDCTout", &[OUT + 0x600 + 768 * s, 192])?;
    }
    state(h, "left over", st)?;
    observe_out(h, "left over: output");
    call(h, "DecDCTinCallback", &[0])?;
    // New tables, then 15-bit frames.
    put_env_tables(h, rng, ENV);
    call(h, "DecDCTPutEnv", &[ENV])?;
    call(h, "DecDCTGetEnv", &[ENV2])?;
    observe_ram(h, "the new tables", ENV2, ENV_LEN);
    put_stream(h, rng, IN, 3, true, Count::Whole);
    dct_in(h, IN, 2)?;
    call(h, "DecDCTout", &[OUT, 3 * 128])?;
    call(h, "DecDCTinSync", &[0])?;
    call(h, "DecDCToutSync", &[0])?;
    state(h, "15-bit", st)?;
    observe_out(h, "15-bit: output");
    // A reset with DMA1 waiting for a short input: zeros, and the
    // callback.
    put_stream(h, rng, IN2, 2, true, Count::Over);
    dct_in(h, IN2, 1)?;
    call(h, "DecDCTout", &[OUT, 768])?;
    state(h, "waiting", st)?;
    call(h, "DecDCTReset", &[1])?;
    state(h, "reset", st)?;
    observe_out(h, "reset: output");
    // A short input waited out.
    call(h, "DecDCTReset", &[0])?;
    put_stream(h, rng, IN, 1, false, Count::Over);
    dct_in(h, IN, 0)?;
    h.vblanks(1)?;
    vcount(h, "before the wait")?;
    call(h, "DecDCTinSync", &[0])?;
    vcount(h, "after the wait")?;
    // DecDCTin after a timeout: its own wait sees nothing.
    put_stream(h, rng, IN2, 1, false, Count::Whole);
    dct_in(h, IN2, 0)?;
    call(h, "DecDCTout", &[OUT, 16])?;
    state(h, "after the timeout", st)?;
    observe_out(h, "after the timeout: output");
    call(h, "DecDCTReset", &[7])?;
    call(h, "DecDCTReset", &[0])?;
    call(h, "DecDCToutCallback", &[0])?;
    call(h, "DecDCTGetEnv", &[ENV2])?;
    observe_ram(h, "tables at the end", ENV2, ENV_LEN);
    h.vblanks(1)?;
    state(h, "end", st)?;
    vcount(h, "end")?;
    Ok(())
}
