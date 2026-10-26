//! The three root counters (psx-spx "Timers"), as a function of virtual
//! time rather than ticked: a counter's value at any cycle follows from
//! when it was last written, its mode and its target.
//!
//! Mode bits used: 3 reset at target (else at 0xFFFF), 4 IRQ at target,
//! 5 IRQ at 0xFFFF, 6 repeat IRQ, 8-9 clock source. Sync (gate) modes are
//! not modelled.

use crate::clock::Video;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Timer {
    pub mode: u16,
    pub target: u16,
    /// Cycle at which the counter was last set, and to what.
    pub base_cycle: u64,
    pub base_value: u16,
    /// A one-shot IRQ has fired.
    pub fired: bool,
}

impl Timer {
    /// Ticks of this counter's clock source elapsed between two cycles.
    fn ticks(n: usize, mode: u16, video: Video, from: u64, to: u64) -> u64 {
        let src = (mode >> 8) & 3;
        match (n, src) {
            (0, 1) | (0, 3) => video.dots(to) - video.dots(from),
            (1, 1) | (1, 3) => video.hblanks(to) - video.hblanks(from),
            (2, 2) | (2, 3) => to / 8 - from / 8,
            _ => to - from,
        }
    }

    /// The cycle at which `ticks` source ticks after `from` have elapsed.
    fn cycle_after(n: usize, mode: u16, video: Video, from: u64, ticks: u64) -> u64 {
        let src = (mode >> 8) & 3;
        match (n, src) {
            (0, 1) | (0, 3) => video.dot_at(video.dots(from) + ticks),
            (1, 1) | (1, 3) => video.hblank_at(video.hblanks(from) + ticks),
            (2, 2) | (2, 3) => (from / 8 + ticks) * 8,
            _ => from + ticks,
        }
    }

    fn period(&self) -> u64 {
        if self.mode & 0x08 != 0 { self.target as u64 + 1 } else { 0x1_0000 }
    }

    /// Counter value at `now`.
    pub fn value(&self, n: usize, video: Video, now: u64) -> u16 {
        let t = Self::ticks(n, self.mode, video, self.base_cycle, now) + self.base_value as u64;
        let p = self.period();
        if (self.base_value as u64) < p {
            return (t % p) as u16;
        }
        // Started above the target: it runs to 0xFFFF and wraps first.
        if t <= 0xFFFF { t as u16 } else { ((t - 0x1_0000) % p) as u16 }
    }

    /// Set the mode register: the counter restarts at 0.
    pub fn write_mode(&mut self, mode: u16, now: u64) {
        self.mode = mode;
        self.base_cycle = now;
        self.base_value = 0;
        self.fired = false;
    }

    pub fn write_count(&mut self, n: usize, video: Video, now: u64, v: u16) {
        let _ = (n, video);
        self.base_cycle = now;
        self.base_value = v;
    }

    /// The next cycle after `now` at which this counter raises its IRQ.
    pub fn next_irq(&self, n: usize, video: Video, now: u64) -> Option<u64> {
        let at_target = self.mode & 0x10 != 0;
        let at_wrap = self.mode & 0x20 != 0;
        if !(at_target || at_wrap) || (self.fired && self.mode & 0x40 == 0) {
            return None;
        }
        let cur = self.value(n, video, now) as u64;
        let p = self.period();
        // Ticks until the counter next equals the target / 0xFFFF.
        let mut best: Option<u64> = None;
        let mut consider = |hit: u64| {
            let d = if hit > cur { hit - cur } else { p - cur + hit };
            best = Some(best.map_or(d, |b| b.min(d)));
        };
        if at_target && (self.target as u64) < p {
            consider(self.target as u64);
        }
        if at_wrap && p == 0x1_0000 {
            consider(0xFFFF);
        }
        best.map(|d| Self::cycle_after(n, self.mode, video, now, d))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_clock_target_reset() {
        let mut t = Timer::default();
        t.write_mode(0x58, 1000); // reset at target, IRQ at target, repeat
        t.target = 99;
        assert_eq!(t.value(2, Video::Ntsc, 1000), 0);
        assert_eq!(t.value(2, Video::Ntsc, 1099), 99);
        assert_eq!(t.value(2, Video::Ntsc, 1100), 0);
        assert_eq!(t.next_irq(2, Video::Ntsc, 1000), Some(1099));
        assert_eq!(t.next_irq(2, Video::Ntsc, 1099), Some(1199));
    }

    #[test]
    fn hblank_source() {
        let mut t = Timer::default();
        t.write_mode(0x100, 0);
        let v = Video::Ntsc;
        assert_eq!(t.value(1, v, v.vblank_at(1)), 262);
        assert_eq!(t.value(1, v, v.hblank_at(263)), 263);
    }

    #[test]
    fn divided_clock() {
        let mut t = Timer::default();
        t.write_mode(0x200, 0);
        assert_eq!(t.value(2, Video::Ntsc, 800), 100);
    }
}
