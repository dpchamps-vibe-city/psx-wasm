//! libapi members that touch hardware or patch the BIOS (D13): `counter.o`
//! (the root counters), `pad.o` (BIOS pad buffers), `patch.o` (pad
//! patches), `first.o` (memory card directory). The other 33 members are
//! BIOS trampolines and run translated, into the runtime's BIOS.
//!
//! `counter.o` follows psyq-decomp `libapi/counter`: `SetRCnt` writes the
//! mode (0x48: reset at target, repeating IRQ; gate, clock source and IRQ
//! enable from the spec's mode bits) and the target; `StartRCnt` unmasks
//! the counter's IRQ.

use crate::{Adapter, M, Result, ret};

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
    ("SetInitPadFlag", nop),
    ("ReadInitPadFlag", nop),
    ("EnablePAD", nop),
    ("DisablePAD", nop),
    ("_patch_pad", nop),
    ("firstfile", firstfile),
];

fn counter(spec: u32) -> Option<usize> {
    let n = (spec & 0xFFFF) as usize;
    (n < 3).then_some(n)
}

fn set_rcnt(m: &mut M, a: [u32; 4]) -> Result {
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
    let now = m.clock.now;
    m.timers[n].target = a[1] as u16;
    m.timers[n].write_mode(mode, now);
    m.reschedule_timer(n);
    ret(1)
}

fn get_rcnt(m: &mut M, a: [u32; 4]) -> Result {
    let Some(n) = counter(a[0]) else { return ret(0) };
    ret(m.timers[n].value(n, m.clock.video, m.clock.now) as u32)
}

fn start_rcnt(m: &mut M, a: [u32; 4]) -> Result {
    let n = (a[0] & 0xFFFF) as usize;
    if n < 3 {
        m.kernel.timer_irq[n] = true;
        m.reschedule_timer(n);
    }
    ret((n < 3) as u32)
}

fn stop_rcnt(m: &mut M, a: [u32; 4]) -> Result {
    let n = (a[0] & 0xFFFF) as usize;
    if n < 3 {
        m.kernel.timer_irq[n] = false;
    }
    ret(1)
}

fn reset_rcnt(m: &mut M, a: [u32; 4]) -> Result {
    let Some(n) = counter(a[0]) else { return ret(0) };
    let (video, now) = (m.clock.video, m.clock.now);
    m.timers[n].write_count(n, video, now, 0);
    m.reschedule_timer(n);
    ret(1)
}

fn init_pad(m: &mut M, a: [u32; 4]) -> Result {
    m.kernel.pad_bufs = Some([(a[0], a[1]), (a[2], a[3])]);
    ret(1)
}

fn start_pad(m: &mut M, _a: [u32; 4]) -> Result {
    m.kernel.pad_started = true;
    ret(1)
}

fn pad_init(m: &mut M, a: [u32; 4]) -> Result {
    // PAD_init(type, buf): a two-port buffer of 0x22 bytes each.
    m.kernel.pad_bufs = Some([(a[1], 0x22), (a[1] + 0x22, 0x22)]);
    m.kernel.pad_started = true;
    ret(1)
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
}
