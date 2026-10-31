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
    ("EnablePAD", enable_pad),
    ("DisablePAD", disable_pad),
    // pad.o's own vblank element's verifier and handler (static).
    ("libapi/pad:0x24c", element_verifier),
    ("libapi/pad:0x1e4", element_handler),
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

/// Root counter `n`'s registers: the count, the mode, the target.
fn rcnt(n: usize, reg: u32) -> u32 {
    0x1F80_1100 + 0x10 * n as u32 + reg
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
    // As counter.o writes them: the mode cleared, the target, the mode.
    set_reg(m, rcnt(n, 4), 2, 0)?;
    set_reg(m, rcnt(n, 8), 2, a[1] & 0xFFFF)?;
    set_reg(m, rcnt(n, 4), 2, mode as u32)?;
    ret(1)
}

fn get_rcnt(m: &mut M, a: [u32; 4]) -> Result {
    let Some(n) = counter(a[0]) else { return ret(0) };
    ret(reg(m, rcnt(n, 0), 2)? & 0xFFFF)
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
    set_reg(m, rcnt(n, 0), 2, 0)?;
    ret(1)
}

/// `InitPAD(buf1, siz1, buf2, siz2)`: `ChangeClearPAD(0)`, then the BIOS's
/// `InitPAD2` (B0:12h); 1. `_patch_pad`, which patches the Sony BIOS's pad
/// handler, has nothing to patch.
fn init_pad(m: &mut M, a: [u32; 4]) -> Result {
    bios(m, 0xB0, 0x5B, [0; 4])?;
    queue_element(m)?;
    bios(m, 0xB0, 0x12, a)?;
    set_init_pad_flag(m)?;
    ret(1)
}

/// `StartPAD`: `StartPAD2` (B0:13h), `ChangeClearPAD(0)`, `EnablePAD`; 1.
fn start_pad(m: &mut M, _a: [u32; 4]) -> Result {
    bios(m, 0xB0, 0x13, [0; 4])?;
    bios(m, 0xB0, 0x5B, [0; 4])?;
    m.kernel.pad_enable = true;
    ret(1)
}

/// `EnablePAD`, `DisablePAD`: the BIOS's pad handler reads the pads, or
/// not. `_patch_pad` points them at the Sony BIOS's `SetPadEnableFlag` and
/// `ClearPadEnableFlag` (psx-spx, "patch_pad_error_handling_and_get_pad_
/// enable_functions"): psx-spx's `pad_enable_flag`.
fn enable_pad(m: &mut M, _a: [u32; 4]) -> Result {
    m.kernel.pad_enable = true;
    ret(1)
}

fn disable_pad(m: &mut M, _a: [u32; 4]) -> Result {
    m.kernel.pad_enable = false;
    ret(1)
}

/// pad.o's own element, which `InitPAD` and `PAD_init` queue at priority
/// 1 (its helper, the `jal` at +4Ch in both): its handler and verifier
/// written in, then out of the chain and back in, in a critical section,
/// as the helper does. Its fields, from the helper's code.
fn queue_element(m: &mut M) -> std::result::Result<(), String> {
    let at = m.sdk.at;
    let err = || format!("libapi/pad at {at:#010x}: no element helper where PsyQ 4.7's object has it");
    let h = crate::libspu::jal(m, at + 0x4C).ok_or_else(err)?;
    let p = |m: &M, o: u32| crate::libspu::pair(m, h + o, h + o + 4).ok_or_else(err);
    let element = p(m, 0x14)?.wrapping_sub(4);
    let (handler, verifier) = (p(m, 0x20)?, p(m, 0x2C)?);
    psx_bios::bios::syscall(m, [1, 0, 0, 0])?;
    for (o, v) in [(4, handler), (8, verifier), (0, 0), (12, 0)] {
        m.mem.w32(element + o, v);
    }
    bios(m, 0xC0, 0x03, [1, element, 0, 0])?;
    bios(m, 0xC0, 0x02, [1, element, 0, 0])?;
    psx_bios::bios::syscall(m, [2, 0, 0, 0])?;
    Ok(())
}

/// The element's verifier: vblank unmasked and pending.
fn element_verifier(m: &mut M, _a: [u32; 4]) -> Result {
    let taken = reg(m, psx_devices::irq::I_MASK, 4)? & reg(m, psx_devices::irq::I_STAT, 4)? & 1;
    ret(taken)
}

/// The element's handler: SIO0's control register cleared, then a
/// countdown from 10: the original's 98 instructions (D71; `pad:0x1e4`,
/// 18 around ten passes of the loop's 8).
fn element_handler(m: &mut M, _a: [u32; 4]) -> Result {
    set_reg(m, 0x1F80_104A, 2, 0)?;
    crate::libpad::spend_in_exception(m, 98);
    ret(0)
}

/// `PAD_init(type, button_dest, …)`: `ChangeClearPAD(0)`, then the BIOS's
/// `PAD_init2` (B0:15h) with the same arguments; pad.o's flag set; 1.
fn pad_init(m: &mut M, a: [u32; 4]) -> Result {
    bios(m, 0xB0, 0x5B, [0; 4])?;
    queue_element(m)?;
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
        let mut m = crate::testing::machine_with_bus();
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
        // Its `jal` to pad.o's element helper, whose pairs name the
        // element (+4) and its handler and verifier.
        let (h, element, handler, verifier) = (0x8004_2CF0, 0x800B_79E8, 0x8004_2D68, 0x8004_2DD0);
        m.mem.w32(at + 0x4C, 0x0C00_0000 | (h & 0x0FFF_FFFF) >> 2);
        for (o, v) in [(0x14, element + 4), (0x20, handler), (0x2C, verifier)] {
            m.mem.w32(h + o, 0x3C02_0000 | (v + 0x8000) >> 16);
            m.mem.w32(h + o + 4, 0x2442_0000 | (v & 0xFFFF));
        }
        m.mem.write(0x8001_0000, &[0x5A; 0x44]);
        assert_eq!(call(&mut m, init_pad, [0x8001_0000, 0x22, 0x8001_0022, 0x22]), 1);
        assert_eq!(m.mem.bytes(0x8001_0000, 0x44), [0; 0x44]);
        assert_eq!(m.mem.r32(flag), 1);
        // Ahead of the kernel's timer elements.
        assert_eq!(m.chain(1)[0], element);
        assert_eq!((m.mem.r32(element + 4), m.mem.r32(element + 8)), (handler, verifier));
        assert_eq!(call(&mut m, start_pad, [0; 4]), 1);
        assert!(m.sio0_started());
        call(&mut m, disable_pad, [0; 4]);
        assert!(!m.kernel.pad_enable);
        call(&mut m, enable_pad, [0; 4]);
        assert!(m.kernel.pad_enable);
    }
}
