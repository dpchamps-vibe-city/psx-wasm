//! Chip irq's cases: libetc's interrupt layer and `VSync`, libapi's root
//! counters (D59; phase 1 task 9a).
//!
//! Observation notes, beyond the module's specs:
//! - **libetc's own addresses.** `ResetCallback`, `StopCallback` and
//!   `RestartCallback` return libetc's context, the same address on both
//!   sides (D67). Cases leave lines 0 and 3 (libetc's own vblank and DMA
//!   handlers) to libetc.
//! - **Hblanks are time.** `VSync(n >= 0)` returns the hblanks since the
//!   last waiting call, exactly: both sides spend the original's
//!   instructions (D71), so their safepoints fall alike.
//! - **Sync before timing.** A case that reads a root counter or counts
//!   timer interrupts lets a vblank pass first, so both sides start from
//!   the same cycle.
//! - **The budget.** After every call and every wait for vblanks, the
//!   guest's budget left: the original's instructions so far, libetc's
//!   dispatcher and handlers among them (D71).
//! - Fake function pointers go only where nothing calls them: lines and
//!   channels that raise nothing in the case.

use super::{Case, Harness, Rng, budget, timed as call, vblanks};

const RCNT: u32 = 0xF200_0000;
/// SetRCnt modes: interrupt at target, system clock (counter 2: not /8).
const RCNT_MD_INTR: u32 = 0x1000;
const RCNT_MD_SC: u32 = 0x0001;
/// Lines no case raises (not vblank or DMA, which are libetc's own).
const QUIET_LINES: [u32; 9] = [1, 2, 4, 5, 6, 7, 8, 9, 10];

pub fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "irq.reset",
            chip: "irq",
            level: 1,
            covers: &["ResetCallback", "CheckCallback", "GetIntrMask"],
            needs: &["ResetCallback", "CheckCallback", "GetIntrMask", "VSync"],
            run: reset,
        },
        Case {
            name: "irq.stop-restart",
            chip: "irq",
            level: 1,
            covers: &["StopCallback", "RestartCallback"],
            needs: &["ResetCallback", "StopCallback", "RestartCallback", "GetIntrMask", "InterruptCallback"],
            run: stop_restart,
        },
        Case {
            name: "irq.interrupt-callback",
            chip: "irq",
            level: 1,
            covers: &["InterruptCallback"],
            needs: &["ResetCallback", "InterruptCallback", "GetIntrMask"],
            run: interrupt_callback,
        },
        Case {
            name: "irq.dma-callback",
            chip: "irq",
            level: 1,
            covers: &["DMACallback", "libetc/intr_dma:0x4c"],
            needs: &["ResetCallback", "DMACallback"],
            run: dma_callback,
        },
        Case {
            name: "irq.vsync-callbacks",
            chip: "irq",
            level: 1,
            covers: &["VSyncCallback", "VSyncCallbacks"],
            needs: &["ResetCallback", "VSyncCallback", "VSyncCallbacks"],
            run: vsync_callbacks,
        },
        Case {
            name: "irq.intr-mask",
            chip: "irq",
            level: 1,
            covers: &["SetIntrMask", "GetIntrMask"],
            needs: &["ResetCallback", "SetIntrMask", "GetIntrMask"],
            run: intr_mask,
        },
        Case { name: "irq.vsync", chip: "irq", level: 1, covers: &["VSync"], needs: &["ResetCallback", "VSync"], run: vsync },
        Case {
            name: "irq.counters",
            chip: "irq",
            level: 1,
            covers: &["SetRCnt", "GetRCnt", "StartRCnt", "StopRCnt", "ResetRCnt"],
            needs: &["SetRCnt", "GetRCnt", "StartRCnt", "StopRCnt", "ResetRCnt", "GetIntrMask"],
            run: counters,
        },
        Case {
            name: "irq.sequence",
            chip: "irq",
            level: 2,
            covers: &[
                "ResetCallback", "VSyncCallback", "VSyncCallbacks", "VSync", "DMACallback", "InterruptCallback", "SetRCnt", "GetRCnt",
                "StartRCnt", "StopRCnt", "ResetRCnt", "SetIntrMask", "GetIntrMask", "CheckCallback",
            ],
            needs: &[
                "ResetCallback", "VSyncCallback", "VSyncCallbacks", "VSync", "DMACallback", "InterruptCallback", "SetRCnt", "GetRCnt",
                "StartRCnt", "StopRCnt", "ResetRCnt", "SetIntrMask", "GetIntrMask", "CheckCallback", "ResetGraph", "ClearOTagR",
                "DrawOTag", "DrawSync",
            ],
            run: sequence,
        },
    ]
}

/// A call whose result is libetc's context pointer (or 0).
fn call_context(h: &mut dyn Harness, name: &str) -> Result<u32, String> {
    let v = h.call(name, &[])?;
    h.observe(&format!("{name}()"), format!("{v:#010x}"));
    budget(h, name);
    Ok(v)
}

/// `VSync(mode)`: Vcount, or the hblanks.
fn vsync_call(h: &mut dyn Harness, mode: i32) -> Result<u32, String> {
    let v = h.call("VSync", &[mode as u32])?;
    let shown = if mode < 0 { format!("Vcount {v}") } else { format!("{v} hblanks") };
    h.observe(&format!("VSync({mode})"), shown);
    budget(h, "VSync");
    Ok(v)
}

/// A function pointer nothing will call.
fn fake(rng: &mut Rng) -> u32 {
    0x8001_0000 | rng.range(1, 0x4000) << 2
}

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

fn reset(h: &mut dyn Harness, _rng: &mut Rng) -> Result<(), String> {
    call(h, "CheckCallback", &[])?;
    call(h, "GetIntrMask", &[])?;
    call_context(h, "ResetCallback")?;
    call(h, "CheckCallback", &[])?;
    call(h, "GetIntrMask", &[])?;
    // Once: a second call changes nothing.
    call_context(h, "ResetCallback")?;
    vsync_call(h, -1)?;
    vblanks(h, 2)?;
    vsync_call(h, -1)?;
    call(h, "GetIntrMask", &[])?;
    Ok(())
}

fn stop_restart(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    call_context(h, "ResetCallback")?;
    call_context(h, "StopCallback")?;
    call(h, "GetIntrMask", &[])?;
    call_context(h, "StopCallback")?;
    // Stopped, the line setter changes nothing.
    let line = QUIET_LINES[rng.range(0, QUIET_LINES.len() as u32) as usize];
    call(h, "InterruptCallback", &[line, fake(rng)])?;
    call_context(h, "RestartCallback")?;
    call(h, "GetIntrMask", &[])?;
    call_context(h, "RestartCallback")?;
    call(h, "InterruptCallback", &[line, 0])?;
    vblanks(h, 1)?;
    call(h, "GetIntrMask", &[])?;
    Ok(())
}

fn interrupt_callback(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    // Before ResetCallback nothing is installed.
    call(h, "InterruptCallback", &[QUIET_LINES[rng.range(0, 9) as usize], fake(rng)])?;
    call(h, "GetIntrMask", &[])?;
    call_context(h, "ResetCallback")?;
    for _ in 0..3 {
        let line = QUIET_LINES[rng.range(0, QUIET_LINES.len() as u32) as usize];
        let (f, g) = (fake(rng), fake(rng));
        call(h, "InterruptCallback", &[line, f])?;
        call(h, "GetIntrMask", &[])?;
        call(h, "InterruptCallback", &[line, f])?;
        call(h, "InterruptCallback", &[line, g])?;
        call(h, "InterruptCallback", &[line, 0])?;
        call(h, "GetIntrMask", &[])?;
    }
    Ok(())
}

fn dma_callback(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    call_context(h, "ResetCallback")?;
    for _ in 0..3 {
        let ch = rng.range(0, 8);
        let f = fake(rng);
        call(h, "DMACallback", &[ch, f])?;
        call(h, "DMACallback", &[ch, f])?;
        call(h, "DMACallback", &[ch, 0])?;
    }
    Ok(())
}

fn vsync_callbacks(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    call_context(h, "ResetCallback")?;
    let (f, g) = (fake(rng), fake(rng));
    call(h, "VSyncCallback", &[f])?;
    call(h, "VSyncCallbacks", &[4, g])?;
    let ch = rng.range(0, 8);
    call(h, "VSyncCallbacks", &[ch, fake(rng)])?;
    call(h, "VSyncCallbacks", &[ch, 0])?;
    call(h, "VSyncCallback", &[0])?;
    call(h, "VSyncCallbacks", &[4, 0])?;
    Ok(())
}

fn intr_mask(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    // I_MASK is the controller's, before libetc too; it keeps 11 bits.
    call(h, "GetIntrMask", &[])?;
    let m = rng.range(0, 0x1_0000);
    call(h, "SetIntrMask", &[m])?;
    call(h, "GetIntrMask", &[])?;
    call(h, "SetIntrMask", &[0])?;
    call_context(h, "ResetCallback")?;
    let m = rng.range(0, 0x1_0000);
    let old = call(h, "SetIntrMask", &[m])?;
    call(h, "GetIntrMask", &[])?;
    call(h, "SetIntrMask", &[old])?;
    call(h, "GetIntrMask", &[])?;
    Ok(())
}

fn vsync(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    call_context(h, "ResetCallback")?;
    // VSync(1) from the start: hblanks since root counter 1 began.
    vsync_call(h, 1)?;
    vblanks(h, rng.range(0, 3))?;
    vsync_call(h, -1)?;
    for _ in 0..4 {
        let mode = [-1, 0, 1, 2, 3, 4][rng.range(0, 6) as usize];
        vsync_call(h, mode)?;
        vsync_call(h, -1)?;
        vblanks(h, rng.range(0, 2))?;
    }
    vsync_call(h, 1)?;
    Ok(())
}

fn counters(h: &mut dyn Harness, rng: &mut Rng) -> Result<(), String> {
    // Without libetc: no other SDK code moves time first.
    let n = rng.range(0, 4);
    let target = rng.range(1, 0x1_0000);
    let mode = [0, RCNT_MD_SC, RCNT_MD_INTR, RCNT_MD_INTR | RCNT_MD_SC, 0x10, 0x10 | RCNT_MD_SC][rng.range(0, 6) as usize];
    call(h, "SetRCnt", &[RCNT | n, target, mode])?;
    call(h, "GetRCnt", &[RCNT | n])?;
    vblanks(h, 1)?;
    call(h, "GetRCnt", &[RCNT | n])?;
    call(h, "ResetRCnt", &[RCNT | n])?;
    call(h, "GetRCnt", &[RCNT | n])?;
    vblanks(h, 1)?;
    call(h, "GetRCnt", &[RCNT | n])?;
    let s = rng.range(0, 7);
    call(h, "StartRCnt", &[RCNT | s])?;
    call(h, "GetIntrMask", &[])?;
    call(h, "StopRCnt", &[RCNT | s])?;
    call(h, "GetIntrMask", &[])?;
    // An out-of-range counter.
    call(h, "SetRCnt", &[RCNT | 3, target, mode])?;
    call(h, "GetRCnt", &[RCNT | 3])?;
    Ok(())
}

/// Init, callbacks on vblank and VSync's waits, vblank masked, a GPU DMA
/// callback around libgpu's own, root counter 2's interrupt.
fn sequence(h: &mut dyn Harness, _rng: &mut Rng) -> Result<(), String> {
    let (cb, state) = callback(h)?;
    call_context(h, "ResetCallback")?;
    call(h, "VSyncCallback", &[cb])?;
    observe_word(h, "callback word", state);
    for f in 0..4 {
        vblanks(h, 1)?;
        vsync_call(h, -1)?;
        observe_word(h, &format!("frame {f}"), state);
    }
    for mode in [0, 2, 1, 3, 0] {
        vsync_call(h, mode)?;
        vsync_call(h, -1)?;
        observe_word(h, &format!("after VSync({mode})"), state);
    }
    call(h, "VSyncCallback", &[0])?;
    vblanks(h, 2)?;
    observe_word(h, "callback removed", state);
    // Vblank masked: libetc counts nothing; unmasked, the pending one.
    call(h, "VSyncCallbacks", &[0, cb])?;
    let mask = call(h, "GetIntrMask", &[])?;
    call(h, "SetIntrMask", &[mask & !1])?;
    vblanks(h, 3)?;
    vsync_call(h, -1)?;
    observe_word(h, "vblank masked", state);
    call(h, "SetIntrMask", &[mask])?;
    vblanks(h, 2)?;
    vsync_call(h, -1)?;
    observe_word(h, "vblank unmasked", state);
    call(h, "VSyncCallbacks", &[0, 0])?;
    call(h, "CheckCallback", &[])?;
    // DMA: libgpu's GPU transfer, with this callback on its channel.
    let ot = super::SCRATCH;
    call(h, "ResetGraph", &[0])?;
    vblanks(h, 1)?;
    call(h, "DMACallback", &[2, cb])?;
    call(h, "ClearOTagR", &[ot, 8])?;
    call(h, "DrawOTag", &[ot + 4 * 7])?;
    call(h, "DrawSync", &[0])?;
    vblanks(h, 1)?;
    observe_word(h, "GPU DMA callback", state);
    call(h, "DMACallback", &[2, 0])?;
    // Root counter 2 every 0x10000 * 8 cycles, its interrupt to the
    // callback, from a vblank.
    vblanks(h, 1)?;
    call(h, "SetRCnt", &[RCNT | 2, 0xFFFF, RCNT_MD_INTR])?;
    call(h, "InterruptCallback", &[6, cb])?;
    call(h, "GetIntrMask", &[])?;
    vblanks(h, 3)?;
    observe_word(h, "root counter 2 interrupts", state);
    // Counter 2 runs at the guest's cycle (D66): exact, now that both sides
    // spend the original's instructions (D71).
    call(h, "GetRCnt", &[RCNT | 2])?;
    call(h, "InterruptCallback", &[6, 0])?;
    call(h, "ResetRCnt", &[RCNT | 2])?;
    call(h, "StartRCnt", &[RCNT | 2])?;
    call(h, "GetIntrMask", &[])?;
    call(h, "StopRCnt", &[RCNT | 2])?;
    call(h, "GetIntrMask", &[])?;
    vblanks(h, 1)?;
    observe_word(h, "callback word at the end", state);
    vsync_call(h, -1)?;
    Ok(())
}
