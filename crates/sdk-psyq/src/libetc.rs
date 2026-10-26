//! libetc: VSync, interrupt and DMA callbacks, the video mode.
//!
//! `VSync` follows the SDK's own routine (psyq-decomp `libetc/2-vsync`): it
//! returns the hblanks counted since the previous call; mode 0 waits for the
//! next vblank, mode n > 0 for the n-th since the previous call, mode < 0
//! returns the vblank count, mode 1 doesn't wait. Callbacks registered here
//! run at the matching virtual-time events.

use crate::{Adapter, M, Result, block, ret};
use psx_bios::{Flow, GuestCall, Wait};

pub const ADAPTERS: &[(&str, Adapter)] = &[
    ("VSync", vsync),
    ("VSyncCallback", vsync_callback),
    ("VSyncCallbacks", vsync_callbacks),
    ("ResetCallback", reset_callback),
    ("StopCallback", stop_callback),
    ("RestartCallback", restart_callback),
    ("CheckCallback", check_callback),
    ("InterruptCallback", interrupt_callback),
    ("DMACallback", dma_callback),
    ("SetIntrMask", set_intr_mask),
    ("GetIntrMask", get_intr_mask),
    ("GetVideoMode", get_video_mode),
    ("SetVideoMode", set_video_mode),
    ("startIntrVSync", nop),
    ("startIntrDMA", nop),
];

#[derive(Debug, Default)]
pub struct Etc {
    /// Vblank count at the previous VSync.
    pub last_vcount: u64,
    /// Hblank count at the previous VSync.
    pub hcount_base: u64,
    /// VSyncCallbacks channels (VSyncCallback is channel 4... in the SDK's
    /// table; all channels run each vblank, in order).
    pub vsync_cbs: [u32; 8],
    /// InterruptCallback per IRQ line.
    pub irq_cbs: [u32; 11],
    /// DMACallback per channel.
    pub dma_cbs: [u32; 7],
    pub mask: u32,
    pub stopped: bool,
    pub video_mode: u32,
    pub reset: bool,
}

pub fn on_vblank(m: &mut M) {
    if m.sdk.etc.stopped || !m.sdk.etc.reset {
        return;
    }
    let cbs: Vec<u32> = std::iter::once(m.sdk.etc.irq_cbs[0]).chain(m.sdk.etc.vsync_cbs).filter(|&f| f != 0).collect();
    m.calls.extend(cbs.into_iter().map(|addr| GuestCall { addr, args: [0; 4] }));
}

/// Queue the callback for DMA channel `ch` (transfer completion).
pub fn dma_done(m: &mut M, ch: usize) {
    let f = m.sdk.etc.dma_cbs[ch];
    if f != 0 && !m.sdk.etc.stopped {
        m.calls.push_back(GuestCall { addr: f, args: [0; 4] });
    }
}

fn vsync(m: &mut M, a: [u32; 4]) -> Result {
    let mode = a[0] as i32;
    let video = m.clock.video;
    let now_h = video.hblanks(m.clock.now);
    let hcount = (now_h.wrapping_sub(m.sdk.etc.hcount_base) & 0xFFFF) as u32;
    if mode < 0 {
        return ret(m.vblanks as u32);
    }
    if mode == 1 {
        return ret(hcount);
    }
    m.count("vsync.waits");
    let last = m.sdk.etc.last_vcount;
    let first = if mode > 0 { last + mode as u64 - 1 } else { last };
    block(Wait::Vblanks(first), move |m: &mut M| {
        let next = m.vblanks + 1;
        Flow::Block {
            until: Wait::Vblanks(next),
            then: Box::new(move |m: &mut M| {
                m.sdk.etc.last_vcount = m.vblanks;
                m.sdk.etc.hcount_base = m.clock.video.hblanks(m.clock.now);
                Flow::ret(hcount)
            }),
        }
    })
}

fn swap(slot: &mut u32, new: u32) -> u32 {
    std::mem::replace(slot, new)
}

fn vsync_callback(m: &mut M, a: [u32; 4]) -> Result {
    ret(swap(&mut m.sdk.etc.vsync_cbs[0], a[0]))
}

fn vsync_callbacks(m: &mut M, a: [u32; 4]) -> Result {
    let ch = (a[0] as usize).min(7);
    ret(swap(&mut m.sdk.etc.vsync_cbs[ch], a[1]))
}

fn reset_callback(m: &mut M, _a: [u32; 4]) -> Result {
    m.sdk.etc.reset = true;
    m.sdk.etc.stopped = false;
    m.sdk.etc.vsync_cbs = [0; 8];
    m.sdk.etc.irq_cbs = [0; 11];
    m.sdk.etc.dma_cbs = [0; 7];
    ret(0)
}

fn stop_callback(m: &mut M, _a: [u32; 4]) -> Result {
    m.sdk.etc.stopped = true;
    ret(0)
}

fn restart_callback(m: &mut M, _a: [u32; 4]) -> Result {
    m.sdk.etc.stopped = false;
    ret(0)
}

fn check_callback(m: &mut M, _a: [u32; 4]) -> Result {
    ret(m.in_irq as u32)
}

fn interrupt_callback(m: &mut M, a: [u32; 4]) -> Result {
    let irq = (a[0] as usize).min(10);
    ret(swap(&mut m.sdk.etc.irq_cbs[irq], a[1]))
}

fn dma_callback(m: &mut M, a: [u32; 4]) -> Result {
    let ch = (a[0] as usize).min(6);
    ret(swap(&mut m.sdk.etc.dma_cbs[ch], a[1]))
}

fn set_intr_mask(m: &mut M, a: [u32; 4]) -> Result {
    ret(swap(&mut m.sdk.etc.mask, a[0] & 0xFFFF))
}

fn get_intr_mask(m: &mut M, _a: [u32; 4]) -> Result {
    ret(m.sdk.etc.mask)
}

fn get_video_mode(m: &mut M, _a: [u32; 4]) -> Result {
    ret(m.sdk.etc.video_mode)
}

fn set_video_mode(m: &mut M, a: [u32; 4]) -> Result {
    ret(swap(&mut m.sdk.etc.video_mode, a[0]))
}

fn nop(_m: &mut M, _a: [u32; 4]) -> Result {
    ret(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{call, machine};

    #[test]
    fn vsync_waits_and_counts_hblanks() {
        let mut m = machine(vec![], Default::default());
        let r = call(&mut m, vsync, [0; 4]);
        assert_eq!(m.vblanks, 1);
        assert_eq!(r, 0);
        // Half a frame later, VSync(1) reports the hblanks so far.
        let half = m.clock.now + (m.clock.video.vblank_at(2) - m.clock.video.vblank_at(1)) / 2;
        m.run_until(half);
        let h = call(&mut m, vsync, [1, 0, 0, 0]);
        assert!((130..=132).contains(&h), "{h}");
        // VSync(2) after that waits for the second vblank since the last.
        call(&mut m, vsync, [2, 0, 0, 0]);
        assert_eq!(m.vblanks, 3);
        assert_eq!(call(&mut m, vsync, [u32::MAX, 0, 0, 0]), 3);
    }

    #[test]
    fn callbacks_run_each_vblank() {
        let mut m = machine(vec![], Default::default());
        call(&mut m, reset_callback, [0; 4]);
        call(&mut m, vsync_callback, [0x8001_1334, 0, 0, 0]);
        call(&mut m, vsync, [0; 4]);
        assert_eq!(m.calls.pop_front().map(|c| c.addr), Some(0x8001_1334));
    }
}
