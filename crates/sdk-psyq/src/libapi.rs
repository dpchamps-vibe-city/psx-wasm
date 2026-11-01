//! libapi members that touch hardware or patch the BIOS (D13): `counter.o`
//! (the root counters), `pad.o` (BIOS pad buffers), `patch.o` (pad
//! patches), `first.o` (memory card directory). The other 33 members are
//! BIOS trampolines and run translated, into the runtime's BIOS.
//!
//! `pad.o`'s own state stays where the object keeps it, found through its
//! code: the element `InitPAD` and `PAD_init` queue at priority 1 (its
//! handler and verifier written in by their helper), the init flag both
//! set at their end through the same `lui`/`sw` pair, and
//! `SetInitPadFlag`/`ReadInitPadFlag`, which only store and load that flag,
//! run as the original code. `EnablePAD` and `DisablePAD` set the BIOS's
//! pad-enable flag, which the native pad handler reads
//! (`psx_bios::kernel`).
//!
//! The BIOS functions these adapters replace are called through their
//! guest-visible entries (`psx_bios::bios::dispatch`): a patched entry runs
//! the guest's own function where the original makes the call, and the
//! adapter follows it with [`psx_bios::machine::seq`]. The operands read
//! from the object are taken before those calls, since a callback may enter
//! another adapter and move `sdk.at` (D61).
//!
//! `counter.o` (chip irq, D59) follows PsyQ 4.7's object: `SetRCnt`
//! writes the mode (0x48: reset at target, repeating IRQ; sync, clock
//! source and IRQ at target from the spec's mode bits) and the target;
//! `StartRCnt` and `StopRCnt` unmask and mask the counter's interrupt in
//! I_MASK (its table: counters 0-2, then RCntCNT3, vblank), which the
//! kernel's root counter handler and libetc's dispatcher both go by.

use crate::{Adapter, M, Psyq, Result, reg, ret, set_reg};
use psx_bios::Flow;

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

/// `InitPAD(buf1, siz1, buf2, siz2)`: pad.o's element queued, the BIOS's
/// `InitPAD2` (B0:12h) with the same arguments, pad.o's flag set; 1.
///
/// The object's operands are taken here, where the runtime points `sdk.at`
/// at this function, and not after the calls: a patched entry's callback
/// may enter another adapter, whose call moves it (D61).
fn init_pad(m: &mut M, a: [u32; 4]) -> Result {
    let at = m.sdk.at;
    let e = Element::find(m, at)?;
    let flag = init_flag(m, at)?;
    let flow = psx_bios::bios::dispatch(m, 0xB0, 0x5B, [0; 4])?;
    Ok(psx_bios::machine::seq(m, flow, move |m, _| {
        let flow = queue_element(m, e);
        psx_bios::machine::seq(m, flow, move |m, _| {
            let flow = psx_bios::bios::dispatch(m, 0xB0, 0x12, a).unwrap_or_else(|e| panic!("InitPAD: {e}"));
            psx_bios::machine::seq(m, flow, move |m, _| {
                m.mem.w32(flag, 1);
                Flow::ret(1)
            })
        })
    }))
}

/// `StartPAD`: `StartPAD2` (B0:13h), `ChangeClearPAD(0)`, `EnablePAD`; 1.
fn start_pad(m: &mut M, _a: [u32; 4]) -> Result {
    let flow = psx_bios::bios::dispatch(m, 0xB0, 0x13, [0; 4])?;
    Ok(psx_bios::machine::seq(m, flow, |m, _| {
        let flow = psx_bios::bios::dispatch(m, 0xB0, 0x5B, [0; 4]).unwrap_or_else(|e| panic!("StartPAD: {e}"));
        psx_bios::machine::seq(m, flow, |m, _| {
            m.kernel.pad_enable = true;
            Flow::ret(1)
        })
    }))
}

/// `EnablePAD`, `DisablePAD`: whether the BIOS's pad handler reads the
/// pads. `_patch_pad` points them at the Sony BIOS's
/// `SetPadEnableFlag` and `ClearPadEnableFlag` (psx-spx, its
/// `pad_enable_flag`), which is the state the native handler goes by.
fn enable_pad(m: &mut M, _a: [u32; 4]) -> Result {
    m.kernel.pad_enable = true;
    ret(1)
}

fn disable_pad(m: &mut M, _a: [u32; 4]) -> Result {
    m.kernel.pad_enable = false;
    ret(1)
}

/// `PAD_init(type, button_dest, …)`: `ChangeClearPAD(0)`, pad.o's element
/// queued, the BIOS's `PAD_init2` (B0:15h) with the same arguments,
/// pad.o's flag set; 1.
fn pad_init(m: &mut M, a: [u32; 4]) -> Result {
    let at = m.sdk.at;
    let e = Element::find(m, at)?;
    let flag = init_flag(m, at)?;
    let flow = psx_bios::bios::dispatch(m, 0xB0, 0x5B, [0; 4])?;
    Ok(psx_bios::machine::seq(m, flow, move |m, _| {
        let flow = queue_element(m, e);
        psx_bios::machine::seq(m, flow, move |m, _| {
            let flow = psx_bios::bios::dispatch(m, 0xB0, 0x15, a).unwrap_or_else(|e| panic!("PAD_init: {e}"));
            psx_bios::machine::seq(m, flow, move |m, _| {
                m.mem.w32(flag, 1);
                Flow::ret(1)
            })
        })
    }))
}

/// pad.o's flag, which `ReadInitPadFlag` reads (libcard's `InitCARD`
/// keeps the pads in the BIOS's handler only when it is set): `PAD_init`
/// and `InitPAD` set it at the end, through the `lui`/`sw` pair at the same
/// place in both, once the BIOS's init call has returned.
fn init_flag(m: &M, at: u32) -> std::result::Result<u32, String> {
    crate::libspu::pair(m, at + 0x6C, at + 0x70)
        .ok_or_else(|| format!("libapi/pad at {at:#010x}: no init flag where PsyQ 4.7's object has it"))
}

/// pad.o's own `SysEnqIntRP` element, as the helper `InitPAD` and
/// `PAD_init` call (the `jal` at +4Ch) names it: the element and the
/// handler and verifier written into it.
#[derive(Clone, Copy)]
struct Element {
    addr: u32,
    handler: u32,
    verifier: u32,
}

impl Element {
    /// The helper's operands in the object at `at`.
    fn find(m: &M, at: u32) -> std::result::Result<Self, String> {
        let err = || format!("libapi/pad at {at:#010x}: no element helper where PsyQ 4.7's object has it");
        let h = crate::libspu::jal(m, at + 0x4C).ok_or_else(err)?;
        let p = |o: u32| crate::libspu::pair(m, h + o, h + o + 4).ok_or_else(err);
        // The helper's pair makes `_apiPadPatch + 4`; the element starts
        // four bytes before it (its `next` word's place).
        Ok(Element { addr: p(0x14)?.wrapping_sub(4), handler: p(0x20)?, verifier: p(0x2C)? })
    }
}

/// pad.o's own `SysEnqIntRP` element, which `InitPAD` and `PAD_init`
/// queue at priority 1 through the helper both call: its handler and
/// verifier written in, its `next` word cleared (the original helper's
/// order), then out of the chain and back in, in a critical section, as
/// the helper does. Its calls are the entry's own: a patched table runs
/// the guest's function where the original makes the call.
fn queue_element(m: &mut M, e: Element) -> Flow<Psyq> {
    psx_bios::bios::syscall(m, [1, 0, 0, 0]).unwrap_or_else(|err| panic!("libapi/pad: {err}"));
    for (o, v) in [(4, e.handler), (8, e.verifier), (0, 0), (12, 0)] {
        m.mem.w32(e.addr + o, v);
    }
    let flow = psx_bios::bios::dispatch(m, 0xC0, 0x03, [1, e.addr, 0, 0]).unwrap_or_else(|err| panic!("libapi/pad: {err}"));
    psx_bios::machine::seq(m, flow, move |m, _| {
        let flow = psx_bios::bios::dispatch(m, 0xC0, 0x02, [1, e.addr, 0, 0]).unwrap_or_else(|err| panic!("libapi/pad: {err}"));
        psx_bios::machine::seq(m, flow, move |m, _| {
            psx_bios::bios::syscall(m, [2, 0, 0, 0]).unwrap_or_else(|err| panic!("libapi/pad: {err}"));
            Flow::ret(0)
        })
    })
}

/// The element's verifier: vblank unmasked and pending.
fn element_verifier(m: &mut M, _a: [u32; 4]) -> Result {
    let taken = reg(m, psx_devices::irq::I_MASK, 4)? & reg(m, psx_devices::irq::I_STAT, 4)? & 1;
    ret(taken)
}

/// The element's handler: SIO0's control cleared, then the countdown the
/// original spins out there (`_apiPadPatch`'s handler, +1E4h).
fn element_handler(m: &mut M, _a: [u32; 4]) -> Result {
    set_reg(m, psx_devices::sio::BASE + 0x0A, 2, 0)?;
    crate::libpad::spend_in_exception(m, 11);
    ret(0)
}

/// libapi's `firstfile` calls the BIOS's `firstfile2` (B0h:42h) and
/// patches a BIOS bug in kernel RAM; the HLE BIOS has no such bug. It
/// calls the entry, so a patch's function runs and its result is what the
/// adapter returns.
fn firstfile(m: &mut M, a: [u32; 4]) -> Result {
    psx_bios::bios::dispatch(m, 0xB0, 0x42, a)
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

    /// pad.o's operands, as btn's link places them: the `lui`/`sw` pair
    /// both functions end with, its helper's `jal`, and the element and
    /// handlers the helper's pairs name.
    fn btn_pad(m: &mut M) {
        let at = AT;
        let (flag, (h, element, handler, verifier)) = (0x8005_E0E4, (0x8004_2CF0, 0x800B_79E8, 0x8004_2D68, 0x8004_2DD0));
        m.sdk.at = at;
        m.mem.w32(at + 0x6C, 0x3C01_0000 | (flag + 0x8000) >> 16);
        m.mem.w32(at + 0x70, 0xAC22_0000 | (flag & 0xFFFF));
        let (hi, lo) = (0x3C02_0000, 0x2442_0000);
        m.mem.w32(at + 0x4C, 0x0C00_0000 | (h & 0x0FFF_FFFF) >> 2);
        for (o, v) in [(0x14, element + 4), (0x20, handler), (0x2C, verifier)] {
            m.mem.w32(h + o, hi | (v + 0x8000) >> 16);
            m.mem.w32(h + o + 4, lo | (v & 0xFFFF));
        }
    }

    const AT: u32 = 0x8004_2C30;
    const FLAG: u32 = 0x8005_E0E4;

    #[test]
    fn pads_through_the_bios() {
        let mut m = machine(vec![], Default::default());
        btn_pad(&mut m);
        m.mem.write(0x8001_0000, &[0x5A; 0x44]);
        assert_eq!(call(&mut m, init_pad, [0x8001_0000, 0x22, 0x8001_0022, 0x22]), 1);
        assert_eq!(m.mem.bytes(0x8001_0000, 0x44), [0; 0x44]);
        assert_eq!(m.mem.r32(FLAG), 1, "pad.o's init flag");
        // Ahead of the kernel's timer elements.
        assert_eq!(m.chain(1)[0], 0x800B_79E8);
        assert_eq!((m.mem.r32(0x800B_79E8 + 4), m.mem.r32(0x800B_79E8 + 8)), (0x8004_2D68, 0x8004_2DD0));
        assert_eq!(call(&mut m, start_pad, [0; 4]), 1);
        assert!(m.sio0_started());
        assert!(m.kernel.pad_enable);
        call(&mut m, disable_pad, [0; 4]);
        assert!(!m.kernel.pad_enable);
        call(&mut m, enable_pad, [0; 4]);
        assert!(m.kernel.pad_enable);
        // PAD_init sets the flag too, through the same pair.
        m.mem.w32(FLAG, 0);
        assert_eq!(call(&mut m, pad_init, [0x8001_0000, 0x22, 0, 0]), 1);
        assert_eq!(m.mem.r32(FLAG), 1);
        assert_eq!(m.chain(1), vec![0x800B_79E8], "queued once, not twice");
    }

    /// Where the guest's patched functions note their turn.
    const LOG: u32 = 0x8002_0000;
    const ELEMENT: u32 = 0x800B_79E8;
    const HANDLER: u32 = 0x8004_2D68;
    const VERIFIER: u32 = 0x8004_2DD0;
    /// Where another adapter's call leaves `sdk.at`.
    const OTHER_AT: u32 = AT + 0x1000;
    const CHANGE_CLEAR_PAD: u32 = 0x8006_0000;
    const DEQUEUE: u32 = 0x8006_0004;
    const ENQUEUE: u32 = 0x8006_0008;
    const INIT_PAD2: u32 = 0x8006_000C;
    const FIRSTFILE: u32 = 0x8006_0010;

    fn note(m: &mut M, v: u32) {
        let n = m.mem.r32(LOG);
        m.mem.w32(LOG + 4 + 4 * n, v);
        m.mem.w32(LOG, n + 1);
    }

    fn notes(m: &M) -> Vec<u32> {
        let n = m.mem.r32(LOG);
        (0..n).map(|i| m.mem.r32(LOG + 4 + 4 * i)).collect()
    }

    /// The arguments the guest call being served got.
    fn args() -> [u32; 4] {
        *crate::testing::irq_args().last().expect("a guest call's arguments")
    }

    /// A patch of `ChangeClearPAD`: guest code that makes a BIOS call,
    /// enters another adapter (the runtime points `sdk.at` at it) and
    /// leaves it there.
    fn change_clear_pad(m: &mut M) {
        note(m, 0x5B);
        assert_eq!(args(), [0; 4]);
        assert!(matches!(psx_bios::bios::call(m, 0xA0, 0x1B, [0x8002_0100, 0, 0, 0]).unwrap(), Flow::Return(3)));
        m.sdk.at = OTHER_AT;
        crate::testing::call(m, disable_pad, [0; 4]);
    }

    /// A patch of `SysDeqIntRP`: the element the helper wrote, its `next`
    /// cleared (the original helper's order), the critical section held.
    fn dequeue(m: &mut M) {
        note(m, 0x03);
        assert_eq!(args(), [1, ELEMENT, 0, 0]);
        assert!(m.crit, "the helper is in its critical section");
        assert_eq!(m.mem.r32(ELEMENT), 0, "the element's next is cleared before the dequeue");
        assert_eq!((m.mem.r32(ELEMENT + 4), m.mem.r32(ELEMENT + 8)), (HANDLER, VERIFIER));
    }

    fn enqueue(m: &mut M) {
        note(m, 0x02);
        assert_eq!(args(), [1, ELEMENT, 0, 0]);
        assert!(m.crit, "the critical section covers the enqueue too");
    }

    /// A patch of `InitPAD2`: the helper is done, so the critical section
    /// is left and the element is the one it queued; the init flag is not
    /// set until this call returns.
    fn init_pad2(m: &mut M) {
        note(m, 0x12);
        assert_eq!(args(), [0x8001_0000, 0x22, 0x8001_0022, 0x22]);
        assert!(!m.crit, "the helper leaves its critical section before the init call");
        assert_eq!((m.mem.r32(ELEMENT), m.mem.r32(ELEMENT + 4), m.mem.r32(ELEMENT + 8)), (0, HANDLER, VERIFIER));
        assert_eq!(m.mem.r32(FLAG), 0, "the init flag waits for the call's return");
    }

    #[test]
    fn patched_pad_calls_follow_the_helper_order() {
        let mut m = machine(vec![], Default::default());
        btn_pad(&mut m);
        m.mem.write(0x8002_0100, b"pad\0");
        for (table, func, addr) in [(0xB0, 0x5B, CHANGE_CLEAR_PAD), (0xC0, 0x03, DEQUEUE), (0xC0, 0x02, ENQUEUE), (0xB0, 0x12, INIT_PAD2)] {
            m.mem.w32(psx_bios::tables::entry(table, func).expect("a table entry"), addr);
        }
        crate::testing::double(CHANGE_CLEAR_PAD, change_clear_pad);
        crate::testing::double(DEQUEUE, dequeue);
        crate::testing::double(ENQUEUE, enqueue);
        crate::testing::double(INIT_PAD2, init_pad2);
        let chain = m.chain(1);
        m.sdk.at = AT;
        assert_eq!(call(&mut m, init_pad, [0x8001_0000, 0x22, 0x8001_0022, 0x22]), 1);
        assert_eq!(notes(&m), [0x5B, 0x03, 0x02, 0x12], "the helper's calls, in its order");
        assert_eq!(m.mem.r32(FLAG), 1, "pad.o's init flag, at the operands' address");
        assert!(!m.crit);
        assert_eq!(m.chain(1), chain, "the guest's enqueue runs, not the native service");
        assert_eq!(m.sdk.at, OTHER_AT, "the callback's adapter call left sdk.at moved");
    }

    #[test]
    fn firstfile_returns_the_patched_calls_result() {
        let mut m = machine(vec![], Default::default());
        m.mem.w32(psx_bios::tables::entry(0xB0, 0x42).expect("the firstfile entry"), FIRSTFILE);
        let a = [0x8001_0000, 0x8005_0000, 0, 0];
        let Flow::Call { call, then } = firstfile(&mut m, a).unwrap() else { panic!("firstfile did not call the entry") };
        assert_eq!(call, psx_bios::GuestCall { addr: FIRSTFILE, args: a });
        let Flow::Return(v) = then(&mut m, 0xFFFF_FFFE) else { panic!("firstfile did not return the call's result") };
        assert_eq!(v, 0xFFFF_FFFE);
    }
}
