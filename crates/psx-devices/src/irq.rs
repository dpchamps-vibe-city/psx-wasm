//! The interrupt controller's registers (psx-spx "Interrupts"): I_STAT at
//! 1F801070h, I_MASK at 1F801074h. Used when original SDK code runs
//! against register-level devices (conformance runs); with adapters, the
//! runtime delivers interrupts as events and nothing reads these.

pub const VBLANK: u32 = 0;
pub const GPU: u32 = 1;
pub const CDROM: u32 = 2;
pub const DMA: u32 = 3;
pub const TIMER0: u32 = 4;
pub const SIO0: u32 = 7;
pub const SPU: u32 = 9;

pub const I_STAT: u32 = 0x1F80_1070;
pub const I_MASK: u32 = 0x1F80_1074;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Irq {
    pub stat: u32,
    pub mask: u32,
}

impl Irq {
    pub fn raise(&mut self, bit: u32) {
        self.stat |= 1 << bit;
    }

    /// An unmasked interrupt is pending (the CPU would take it).
    pub fn pending(&self) -> bool {
        self.stat & self.mask & 0x7FF != 0
    }

    /// Register `addr` (I_STAT or I_MASK, any byte lane).
    pub fn handles(addr: u32) -> bool {
        (I_STAT..I_MASK + 4).contains(&(addr | 0x1F00_0000))
    }

    pub fn read(&self, addr: u32) -> u32 {
        let reg = if (addr | 0x1F00_0000) & !3 == I_STAT { self.stat } else { self.mask };
        reg >> (8 * (addr & 3))
    }

    /// Writing I_STAT acknowledges: bits written as 0 are cleared.
    /// Writing I_MASK sets it.
    pub fn write(&mut self, addr: u32, size: u32, v: u32) {
        let shift = 8 * (addr & 3);
        let lane = if size >= 4 { u32::MAX } else { ((1u32 << (8 * size)) - 1) << shift };
        let v = (v << shift) & lane;
        if (addr | 0x1F00_0000) & !3 == I_STAT {
            self.stat &= v | !lane;
        } else {
            self.mask = (self.mask & !lane) | (v & 0x7FF);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acknowledge_and_mask() {
        let mut i = Irq::default();
        i.raise(VBLANK);
        i.raise(SIO0);
        assert!(!i.pending());
        i.write(I_MASK, 4, 1);
        assert!(i.pending());
        i.write(I_STAT, 4, !1);
        assert_eq!(i.stat, 1 << SIO0);
        assert!(!i.pending());
        i.write(I_STAT, 2, 0);
        assert_eq!(i.stat, 0);
        assert_eq!(i.read(I_MASK), 1);
    }
}
