//! Virtual time (docs/design.md §6): CPU cycles at 33.8688 MHz, advanced
//! only by safepoints and blocking calls, with an ordered queue of device
//! events. Vblank and hblank times are exact rationals of the video clock
//! (psx-spx "GPU Video Timings"), so frames never drift.

use std::collections::BTreeMap;

pub const CPU_HZ: u64 = 33_868_800;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Video {
    #[default]
    Ntsc,
    Pal,
}

impl Video {
    pub fn video_hz(self) -> u64 {
        match self {
            Video::Ntsc => 53_693_175,
            Video::Pal => 53_203_425,
        }
    }

    /// Video clocks per scanline.
    pub fn dots_per_line(self) -> u64 {
        match self {
            Video::Ntsc => 3413,
            Video::Pal => 3406,
        }
    }

    /// Scanlines per (non-interlaced) frame.
    pub fn lines(self) -> u64 {
        match self {
            Video::Ntsc => 263,
            Video::Pal => 314,
        }
    }

    /// CPU cycle at which vblank `n` (1-based) begins: frame `n` ends.
    pub fn vblank_at(self, n: u64) -> u64 {
        (n as u128 * (self.dots_per_line() * self.lines()) as u128 * CPU_HZ as u128 / self.video_hz() as u128) as u64
    }

    /// Hblanks (scanlines) elapsed by CPU cycle `t`.
    pub fn hblanks(self, t: u64) -> u64 {
        (t as u128 * self.video_hz() as u128 / (self.dots_per_line() as u128 * CPU_HZ as u128)) as u64
    }

    /// CPU cycle at which hblank `n` occurs.
    pub fn hblank_at(self, n: u64) -> u64 {
        (n as u128 * (self.dots_per_line() as u128 * CPU_HZ as u128)).div_ceil(self.video_hz() as u128) as u64
    }

    /// Video clocks (dots) elapsed by CPU cycle `t`.
    pub fn dots(self, t: u64) -> u64 {
        (t as u128 * self.video_hz() as u128 / CPU_HZ as u128) as u64
    }

    /// CPU cycle at which dot `n` occurs.
    pub fn dot_at(self, n: u64) -> u64 {
        (n as u128 * CPU_HZ as u128).div_ceil(self.video_hz() as u128) as u64
    }
}

/// The timeline: current time and the queue of pending events, delivered
/// in (time, scheduling order).
#[derive(Debug, Clone)]
pub struct Clock<E> {
    pub now: u64,
    pub video: Video,
    events: BTreeMap<(u64, u64), E>,
    seq: u64,
}

impl<E: Clone + PartialEq> Clock<E> {
    pub fn new(video: Video) -> Self {
        Self { now: 0, video, events: BTreeMap::new(), seq: 0 }
    }

    pub fn schedule(&mut self, at: u64, e: E) {
        self.seq += 1;
        self.events.insert((at.max(self.now), self.seq), e);
    }

    pub fn cancel(&mut self, pred: impl Fn(&E) -> bool) {
        self.events.retain(|_, e| !pred(e));
    }

    pub fn pending(&self, pred: impl Fn(&E) -> bool) -> Option<u64> {
        self.events.iter().find(|(_, e)| pred(e)).map(|((t, _), _)| *t)
    }

    pub fn next_time(&self) -> Option<u64> {
        self.events.keys().next().map(|(t, _)| *t)
    }

    /// The next event due at or before `until`, advancing `now` to it.
    pub fn pop_until(&mut self, until: u64) -> Option<E> {
        let (&key, _) = self.events.iter().next()?;
        if key.0 > until {
            return None;
        }
        let e = self.events.remove(&key)?;
        self.now = self.now.max(key.0);
        Some(e)
    }

    /// Move time forward with no event in between (callers drain first).
    pub fn advance_to(&mut self, t: u64) {
        self.now = self.now.max(t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_rates() {
        // From psx-spx's dots per line and lines per frame. Its rate table
        // says 59.826 / 49.761 Hz, 0.015–0.03% off its own clock figures; the
        // timer-consistent derivation is used, and the device oracle can
        // measure the true interval.
        let ntsc = CPU_HZ as f64 / Video::Ntsc.vblank_at(1) as f64;
        let pal = CPU_HZ as f64 / Video::Pal.vblank_at(1) as f64;
        assert!((ntsc - 59.817).abs() < 0.001, "{ntsc}");
        assert!((pal - 49.747).abs() < 0.001, "{pal}");
        // No drift: 60,000 frames is an exact rational.
        assert_eq!(Video::Ntsc.vblank_at(60_000), (60_000u128 * 897_619 * 33_868_800 / 53_693_175) as u64);
        assert_eq!(Video::Ntsc.hblanks(Video::Ntsc.vblank_at(1)), 262);
        assert_eq!(Video::Ntsc.hblanks(Video::Ntsc.hblank_at(263)), 263);
    }

    #[test]
    fn events_in_order() {
        let mut c: Clock<u32> = Clock::new(Video::Ntsc);
        c.schedule(100, 1);
        c.schedule(50, 2);
        c.schedule(100, 3);
        assert_eq!(c.pop_until(60), Some(2));
        assert_eq!(c.now, 50);
        assert_eq!(c.pop_until(60), None);
        assert_eq!(c.pop_until(200), Some(1));
        assert_eq!(c.pop_until(200), Some(3));
    }
}
