//! libapi members that touch hardware or patch the BIOS (D13): `counter.o`
//! (the root counters), `pad.o` (BIOS pad buffers), `patch.o` (pad
//! patches), `first.o` (memory card directory). The other 33 members are
//! BIOS trampolines and run translated, into the runtime's BIOS.
//!
//! `counter.o` (chip irq, D59) follows PsyQ 4.7's object: `SetRCnt`
//! writes the mode (0x48: reset at target, repeating IRQ; sync, clock
//! source and IRQ at target from the spec's mode bits) and the target;
//! `StartRCnt` and `StopRCnt` unmask and mask the counter's interrupt in
//! I_MASK (its table: counters 0-2, then RCntCNT3, vblank), which the
//! kernel's root counter handler and libetc's dispatcher both go by.

use crate::{Adapter, M, Result, bios, reg, ret, set_reg};

/// The root-counter adapters (`counter.o`): chip irq (D59).
pub const COUNTERS: &[&str] = &["SetRCnt", "GetRCnt", "StartRCnt", "StopRCnt", "ResetRCnt"];

pub const ADAPTERS: &[(&str, Adapter)] = &[
    ("SetRCnt", set_rcnt),
    ("GetRCnt", get_rcnt),
    ("StartRCnt", start_rcnt),
    ("StopRCnt", stop_rcnt),
    ("ResetRCnt", reset_rcnt),
    ("InitPAD", init_pad),
    ("StartPAD", start_pad),
    ("PAD_init", pad_init),
    ("EnablePAD", nop),
    ("DisablePAD", nop),
    ("_patch_pad", nop),
    ("firstfile", firstfile),
];

/// `counter.o`'s I_MASK bits per counter spec (its table runs on into
/// zeros).
const RCNT_IRQ: [u32; 4] = [0x10, 0x20, 0x40, 0x01];

fn counter(spec: u32) -> Option<usize> {
    let n = (spec & 0xFFFF) as usize;
    (n < 3).then_some(n)
}

pub(crate) fn set_rcnt(m: &mut M, a: [u32; 4]) -> Result {
    let Some(n) = counter(a[0]) else { return ret(0) };
    let mode_in = a[2];
    let mut mode: u16 = 0x48;
    if n < 2 {
        if mode_in & 0x10 != 0 {
            mode = 0x49;
        }
        if mode_in & 1 == 0 {
            mode |= 0x100;
        }
    } else if mode_in & 1 == 0 {
        mode = 0x248;
    }
    if mode_in & 0x1000 != 0 {
        mode |= 0x10;
    }
    let now = m.timer_now(n);
    m.timers[n].target = a[1] as u16;
    m.timers[n].write_mode(mode, now);
    m.reschedule_timer(n);
    ret(1)
}

fn get_rcnt(m: &mut M, a: [u32; 4]) -> Result {
    let Some(n) = counter(a[0]) else { return ret(0) };
    ret(m.timers[n].value(n, m.clock.video, m.timer_now(n)) as u32)
}

fn start_rcnt(m: &mut M, a: [u32; 4]) -> Result {
    let n = (a[0] & 0xFFFF) as usize;
    let mask = reg(m, psx_devices::irq::I_MASK, 4)?;
    set_reg(m, psx_devices::irq::I_MASK, 4, mask | RCNT_IRQ.get(n).copied().unwrap_or(0))?;
    ret((n < 3) as u32)
}

fn stop_rcnt(m: &mut M, a: [u32; 4]) -> Result {
    let n = (a[0] & 0xFFFF) as usize;
    let mask = reg(m, psx_devices::irq::I_MASK, 4)?;
    set_reg(m, psx_devices::irq::I_MASK, 4, mask & !RCNT_IRQ.get(n).copied().unwrap_or(0))?;
    ret(1)
}

fn reset_rcnt(m: &mut M, a: [u32; 4]) -> Result {
    let Some(n) = counter(a[0]) else { return ret(0) };
    let (video, now) = (m.clock.video, m.timer_now(n));
    m.timers[n].write_count(n, video, now, 0);
    m.reschedule_timer(n);
    ret(1)
}

/// `InitPAD(buf1, siz1, buf2, siz2)`: `ChangeClearPAD(0)`, then the BIOS's
/// `InitPAD2` (B0:12h); 1. `_patch_pad`, which patches the Sony BIOS's pad
/// handler, has nothing to patch.
fn init_pad(m: &mut M, a: [u32; 4]) -> Result {
    bios(m, 0xB0, 0x5B, [0; 4])?;
    bios(m, 0xB0, 0x12, a)?;
    set_init_pad_flag(m)?;
    ret(1)
}

/// `StartPAD`: `StartPAD2` (B0:13h), `ChangeClearPAD(0)`; 1. `EnablePAD`
/// jumps into the patched BIOS.
fn start_pad(m: &mut M, _a: [u32; 4]) -> Result {
    bios(m, 0xB0, 0x13, [0; 4])?;
    bios(m, 0xB0, 0x5B, [0; 4])?;
    ret(1)
}

/// `PAD_init(type, button_dest, …)`: `ChangeClearPAD(0)`, then the BIOS's
/// `PAD_init2` (B0:15h) with the same arguments; pad.o's flag set; 1.
fn pad_init(m: &mut M, a: [u32; 4]) -> Result {
    bios(m, 0xB0, 0x5B, [0; 4])?;
    bios(m, 0xB0, 0x15, a)?;
    set_init_pad_flag(m)?;
    ret(1)
}

/// pad.o's flag, which `ReadInitPadFlag` reads (libcard's `InitCARD`
/// keeps the pads in the BIOS's handler only when it is set): `PAD_init`
/// and `InitPAD` set it at the end, through the `lui`/`sw` pair at the same
/// place in both.
fn set_init_pad_flag(m: &mut M) -> std::result::Result<(), String> {
    let at = m.sdk.at;
    let flag = crate::libspu::pair(m, at + 0x6C, at + 0x70).ok_or_else(|| format!("libapi/pad at {at:#010x}: no init flag where PsyQ 4.7's object has it"))?;
    m.mem.w32(flag, 1);
    Ok(())
}

/// libapi's `firstfile` calls the BIOS's `firstfile2` (B0h:42h) and
/// patches a BIOS bug in kernel RAM; the HLE BIOS has no such bug.
fn firstfile(m: &mut M, a: [u32; 4]) -> Result {
    psx_bios::bios::call(m, 0xB0, 0x42, a)
}

fn nop(_m: &mut M, _a: [u32; 4]) -> Result {
    ret(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{call, machine};

    #[test]
    fn counters() {
        let mut m = machine(vec![], Default::default());
        // RCntCNT2, target 0x1000, RCntMdINTR | RCntMdSC (system clock).
        assert_eq!(call(&mut m, set_rcnt, [0xF200_0002, 0x1000, 0x1001, 0]), 1);
        assert_eq!(m.timers[2].mode, 0x58);
        m.run_until(0x800);
        assert_eq!(call(&mut m, get_rcnt, [0xF200_0002, 0, 0, 0]), 0x800);
        // Without RCntMdSC counter 2 runs at system/8.
        call(&mut m, set_rcnt, [0xF200_0002, 0x1000, 0x1000, 0]);
        assert_eq!(m.timers[2].mode, 0x258);
        assert_eq!(call(&mut m, get_rcnt, [3, 0, 0, 0]), 0);
    }

    #[test]
    fn pads_through_the_bios() {
        let mut m = machine(vec![], Default::default());
        // InitPAD's `lui at, %hi(flag)` / `sw v0, %lo(flag)(at)`.
        let (at, flag) = (0x8004_2C30, 0x8005_E0E4);
        m.sdk.at = at;
        m.mem.w32(at + 0x6C, 0x3C01_0000 | (flag + 0x8000) >> 16);
        m.mem.w32(at + 0x70, 0xAC22_0000 | (flag & 0xFFFF));
        m.mem.write(0x8001_0000, &[0x5A; 0x44]);
        assert_eq!(call(&mut m, init_pad, [0x8001_0000, 0x22, 0x8001_0022, 0x22]), 1);
        assert_eq!(m.mem.bytes(0x8001_0000, 0x44), [0; 0x44]);
        assert_eq!(m.mem.r32(flag), 1);
        assert_eq!(call(&mut m, start_pad, [0; 4]), 1);
        assert!(m.kernel.pad_started);
    }
}
