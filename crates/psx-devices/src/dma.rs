//! The DMA controller's registers (psx-spx "DMA Channels"): per channel
//! MADR/BCR/CHCR at 1F801080h + 10h×ch, DPCR at 1F8010F0h, DICR at
//! 1F8010F4h. Transfers themselves run where memory and the devices are
//! (`psx-bios::io`), instantly in virtual time; this module decides when
//! a channel starts and what completion does to DICR.

pub const BASE: u32 = 0x1F80_1080;
pub const DPCR: u32 = 0x1F80_10F0;
pub const DICR: u32 = 0x1F80_10F4;
pub const GPU: usize = 2;
pub const OTC: usize = 6;

/// CHCR fields.
pub const CHCR_FROM_RAM: u32 = 1;
pub const CHCR_STEP_BACK: u32 = 1 << 1;
pub const CHCR_BUSY: u32 = 1 << 24;
pub const CHCR_TRIGGER: u32 = 1 << 28;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dma {
    pub madr: [u32; 7],
    pub bcr: [u32; 7],
    pub chcr: [u32; 7],
    pub dpcr: u32,
    pub dicr: u32,
}

impl Default for Dma {
    fn default() -> Self {
        Dma { madr: [0; 7], bcr: [0; 7], chcr: [0; 7], dpcr: 0x0765_4321, dicr: 0 }
    }
}

impl Dma {
    pub fn handles(addr: u32) -> bool {
        (BASE..DICR + 8).contains(&(addr | 0x1F00_0000))
    }

    /// CHCR bits 9-10.
    pub fn sync_mode(&self, ch: usize) -> u32 {
        (self.chcr[ch] >> 9) & 3
    }

    fn master(&self) -> bool {
        let d = self.dicr;
        d & (1 << 15) != 0 || (d & (1 << 23) != 0 && (d >> 24) & (d >> 16) & 0x7F != 0)
    }

    pub fn read(&self, addr: u32) -> u32 {
        let a = (addr | 0x1F00_0000) & !3;
        let v = match a {
            DPCR => self.dpcr,
            DICR => (self.dicr & 0x7FFF_FFFF) | (self.master() as u32) << 31,
            _ => {
                let ch = ((a - BASE) >> 4) as usize;
                match (a >> 2) & 3 {
                    0 => self.madr[ch],
                    1 => self.bcr[ch],
                    2 => self.chcr[ch],
                    _ => 0,
                }
            }
        };
        v >> (8 * (addr & 3))
    }

    /// A register write; returns the channel to run now, if any.
    pub fn write(&mut self, addr: u32, size: u32, v: u32) -> Option<usize> {
        let a = (addr | 0x1F00_0000) & !3;
        let shift = 8 * (addr & 3);
        let lane = if size >= 4 { u32::MAX } else { ((1u32 << (8 * size)) - 1) << shift };
        let merge = |old: u32| (old & !lane) | ((v << shift) & lane);
        match a {
            DPCR => {
                self.dpcr = merge(self.dpcr);
                // Enabling a channel starts a transfer that was waiting.
                (0..7).find(|&ch| self.ready(ch))
            }
            DICR => {
                // A flag is acknowledged by a 1 written to it, not by a
                // byte store elsewhere in the register (libcd's streaming
                // ring sets channel 3's enable with one to DICR+2).
                let ack = (v << shift) & lane & 0x7F00_0000;
                self.dicr = (merge(self.dicr) & 0x00FF_803F) | (self.dicr & 0x7F00_0000 & !ack);
                None
            }
            _ => {
                let ch = ((a - BASE) >> 4) as usize;
                match (a >> 2) & 3 {
                    0 => self.madr[ch] = merge(self.madr[ch]) & 0x00FF_FFFF,
                    1 => self.bcr[ch] = merge(self.bcr[ch]),
                    2 => {
                        self.chcr[ch] = merge(self.chcr[ch]);
                        if ch == OTC {
                            // OTC only ever writes backwards to RAM.
                            self.chcr[ch] = (self.chcr[ch] & 0x5100_0000) | CHCR_STEP_BACK;
                        }
                    }
                    _ => {}
                }
                self.ready(ch).then_some(ch)
            }
        }
    }

    /// Started (and triggered, in manual mode) and enabled in DPCR.
    pub fn ready(&self, ch: usize) -> bool {
        let c = self.chcr[ch];
        c & CHCR_BUSY != 0 && (self.sync_mode(ch) != 0 || c & CHCR_TRIGGER != 0) && self.dpcr & (8 << (4 * ch)) != 0
    }

    /// End a channel's transfer; true when the IRQ line rises (IRQ3).
    pub fn complete(&mut self, ch: usize) -> bool {
        self.chcr[ch] &= !(CHCR_BUSY | CHCR_TRIGGER);
        let before = self.master();
        if self.dicr & (1 << (16 + ch)) != 0 {
            self.dicr |= 1 << (24 + ch);
        }
        !before && self.master()
    }

    /// Words of a block or manual transfer (BCR: block size × blocks).
    pub fn words(&self, ch: usize) -> u32 {
        let b = self.bcr[ch];
        let size = if b & 0xFFFF == 0 { 0x1_0000 } else { b & 0xFFFF };
        match self.sync_mode(ch) {
            0 => size,
            _ => size * ((b >> 16).max(1)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_needs_enable_and_completion_flags() {
        let mut d = Dma { dpcr: 0, ..Default::default() };
        assert_eq!(d.write(BASE + 0x28, 4, 0x0100_0401), None, "GPU disabled in DPCR");
        assert_eq!(d.write(DPCR, 4, 0x800), Some(GPU));
        d.write(DICR, 4, 1 << 23 | 1 << 18);
        assert!(d.complete(GPU));
        assert_eq!(d.read(DICR) >> 24, 0x84);
        d.write(DICR + 2, 1, 0x80 | 1 << 2 | 1 << 3);
        assert_eq!(d.read(DICR) >> 24, 0x84, "a byte store to the enables acknowledges nothing");
        d.write(DICR, 4, 1 << 23 | 1 << 18 | 1 << 26);
        assert_eq!(d.read(DICR) >> 24, 0);
        assert_eq!(d.chcr[GPU] & CHCR_BUSY, 0);
    }

    #[test]
    fn otc_is_backwards_and_manual_needs_trigger() {
        let mut d = Dma { dpcr: 0x0800_0000, ..Default::default() };
        assert_eq!(d.write(BASE + 0x68, 4, 0x0100_0000), None, "manual mode without trigger");
        assert_eq!(d.write(BASE + 0x68, 4, 0x1100_0000), Some(OTC));
        assert_eq!(d.chcr[OTC] & CHCR_STEP_BACK, CHCR_STEP_BACK);
        d.bcr[OTC] = 0;
        assert_eq!(d.words(OTC), 0x1_0000);
    }
}
