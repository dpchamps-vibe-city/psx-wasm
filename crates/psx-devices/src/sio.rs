//! SIO0, the controller and memory card port (psx-spx "Controllers and
//! Memory Cards"), at register level: JOY_DATA/STAT/MODE/CTRL/BAUD at
//! 1F801040h-1F80104Eh. Used when original SDK code (libpad, the BIOS pad
//! driver) runs against devices, as in conformance runs; with adapters
//! the pad state goes straight to the SDK's buffers.
//!
//! Timing follows the emulator oracle (Beetle PSX `frontio.c`): a byte
//! takes 8 bit times of `max(0x20, baud << shift[mode & 3])` cycles; a
//! device that acknowledges pulses /ACK 0x40 cycles after the byte, for 32
//! cycles, which raises IRQ7 when JOY_CTRL bit 12 is set. State is a
//! function of virtual time, brought up to date at each access.
//!
//! The digital pad answers as Beetle's `InputDevice_Gamepad` does: `01h`
//! selects it (reply Hi-Z), the command byte gets ID `41h`, and only
//! command `42h` continues with `5Ah` and the two button bytes, the last
//! without /ACK.

use crate::irq::{self, Irq};

pub const BASE: u32 = 0x1F80_1040;
const SCALE_SHIFT: [u32; 4] = [0, 0, 4, 6];
const ACK_DELAY: u64 = 0x40;
const ACK_LEN: u64 = 32;

/// A digital pad on one slot (`None` buttons: nothing connected).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pad {
    /// The two button bytes as sent on the wire (active low).
    pub buttons: Option<[u8; 2]>,
    phase: i8,
    tx: [u8; 3],
    tpos: usize,
    tcount: usize,
}

impl Pad {
    fn select(&mut self) {
        self.phase = 0;
        self.tpos = 0;
        self.tcount = 0;
    }

    /// One byte each way: the reply, and whether the pad acknowledges.
    pub fn exchange(&mut self, host: u8) -> (u8, bool) {
        let Some(buttons) = self.buttons else { return (0xFF, false) };
        let reply = if self.tcount > 0 { self.tx[self.tpos] } else { 0xFF };
        if self.tcount > 0 {
            self.tpos += 1;
            self.tcount -= 1;
        }
        match self.phase {
            0 if host == 0x01 => {
                self.tx[0] = 0x41;
                self.tpos = 0;
                self.tcount = 1;
                self.phase = 1;
            }
            0 => self.phase = -1,
            1 => {
                self.phase = 2;
                self.tx[0] = 0x5A;
                if host == 0x42 {
                    self.tx[1] = buttons[0];
                    self.tx[2] = buttons[1];
                    self.tpos = 0;
                    self.tcount = 3;
                } else {
                    self.phase = -1;
                    self.tcount = 0;
                }
            }
            _ => {}
        }
        (reply, self.tcount > 0)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Sio0 {
    pub mode: u16,
    pub ctrl: u16,
    pub baud: u16,
    pub pads: [Pad; 2],
    /// A byte in flight: (start cycle, host byte).
    transfer: Option<(u64, u8)>,
    rx: u8,
    rx_avail: bool,
    /// /ACK pulse start, and whether its IRQ has been raised.
    ack_at: Option<u64>,
    ack_raised: bool,
    istatus: bool,
    /// Bytes exchanged: (slot, host byte, reply, ack), for decoders.
    pub log: Vec<(u8, u8, u8, bool)>,
}

impl Sio0 {
    pub fn handles(addr: u32) -> bool {
        (BASE..BASE + 0x10).contains(&(addr | 0x1F00_0000))
    }

    fn slot(&self) -> Option<usize> {
        (self.ctrl & 2 != 0).then_some(((self.ctrl >> 13) & 1) as usize)
    }

    fn byte_time(&self) -> u64 {
        8 * ((self.baud as u64) << SCALE_SHIFT[(self.mode & 3) as usize] & !1).max(0x20)
    }

    /// Bring the port up to `now`.
    pub fn update(&mut self, now: u64, irq: &mut Irq) {
        if let Some((start, host)) = self.transfer {
            let end = start + self.byte_time();
            if now >= end {
                self.transfer = None;
                let (reply, ack) = match self.slot() {
                    Some(s) => self.pads[s].exchange(host),
                    None => (0xFF, false),
                };
                self.log.push((self.slot().unwrap_or(0) as u8, host, reply, ack));
                self.rx = reply;
                self.rx_avail = true;
                if self.ctrl & 0x800 != 0 {
                    self.raise(irq);
                }
                if ack {
                    self.ack_at = Some(end + ACK_DELAY);
                    self.ack_raised = false;
                }
            }
        }
        if let Some(at) = self.ack_at {
            if now >= at && !self.ack_raised {
                self.ack_raised = true;
                if self.ctrl & 0x1000 != 0 {
                    self.raise(irq);
                }
            }
        }
    }

    fn raise(&mut self, irq: &mut Irq) {
        self.istatus = true;
        irq.raise(irq::SIO0);
    }

    fn stat(&self, now: u64) -> u32 {
        let mut v = 0;
        if self.transfer.is_none() {
            v |= 1 | 4;
        }
        if self.rx_avail {
            v |= 2;
        }
        if self.ack_at.is_some_and(|at| now >= at && now < at + ACK_LEN) {
            v |= 0x80;
        }
        if self.istatus {
            v |= 0x200;
        }
        v
    }

    pub fn read(&mut self, addr: u32, now: u64, irq: &mut Irq) -> u32 {
        self.update(now, irq);
        match (addr | 0x1F00_0000) - BASE {
            0..=3 => {
                let b = self.rx as u32;
                self.rx_avail = false;
                b * 0x0101_0101
            }
            o @ 4..=7 => self.stat(now) >> (8 * (o - 4)),
            8 => self.mode as u32 | (self.ctrl as u32) << 16,
            9 => (self.mode >> 8) as u32,
            0xA => self.ctrl as u32,
            0xB => (self.ctrl >> 8) as u32,
            0xE => self.baud as u32,
            0xF => (self.baud >> 8) as u32,
            _ => 0,
        }
    }

    pub fn write(&mut self, addr: u32, size: u32, v: u32, now: u64, irq: &mut Irq) {
        self.update(now, irq);
        match (addr | 0x1F00_0000) - BASE {
            0 => {
                if self.ctrl & 1 != 0 {
                    self.transfer = Some((now, v as u8));
                    self.ack_at = None;
                }
            }
            8 => {
                self.mode = (v & 0x013F) as u16;
                if size == 4 {
                    self.write_ctrl((v >> 16) as u16);
                }
            }
            0xA => self.write_ctrl(v as u16),
            0xE => self.baud = v as u16,
            _ => {}
        }
    }

    fn write_ctrl(&mut self, v: u16) {
        let was = self.slot();
        self.ctrl = v & 0x3F2F;
        // Acknowledge and reset lower the IRQ line; I_STAT keeps the edge
        // it latched until software clears it there.
        if v & 0x10 != 0 {
            self.istatus = false;
        }
        if v & 0x40 != 0 {
            let pads = self.pads;
            let log = std::mem::take(&mut self.log);
            *self = Sio0 { pads, log, ..Default::default() };
        }
        let now = self.slot();
        if now != was {
            self.ack_at = None;
            if let Some(s) = now {
                self.pads[s].select();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One exchange as libpad does it: write TX, wait, read RX and STAT.
    fn xfer(s: &mut Sio0, irq: &mut Irq, now: &mut u64, b: u8) -> (u8, bool) {
        s.write(BASE, 1, b as u32, *now, irq);
        *now += s.byte_time() + ACK_DELAY + 1;
        let ack = s.read(BASE + 4, *now, irq) & 0x80 != 0;
        let rx = s.read(BASE, *now, irq) as u8;
        s.write(BASE + 0xA, 2, s.ctrl as u32 | 0x10, *now, irq);
        (rx, ack)
    }

    #[test]
    fn digital_pad_poll() {
        let mut s = Sio0::default();
        let mut irq = Irq::default();
        s.pads[0].buttons = Some([0xF7, 0xFF]);
        s.baud = 0x88;
        s.mode = 0x0D;
        let mut now = 0;
        s.write(BASE + 0xA, 2, 0x1003, now, &mut irq);
        let got: Vec<(u8, bool)> = [0x01, 0x42, 0x00, 0x00, 0x00].iter().map(|&b| xfer(&mut s, &mut irq, &mut now, b)).collect();
        assert_eq!(got, [(0xFF, true), (0x41, true), (0x5A, true), (0xF7, true), (0xFF, false)]);
        assert_ne!(irq.stat & 1 << irq::SIO0, 0, "ACK raised IRQ7");
    }

    #[test]
    fn unknown_command_ends_the_exchange() {
        let mut s = Sio0::default();
        let mut irq = Irq::default();
        s.pads[0].buttons = Some([0xFF, 0xFF]);
        let mut now = 0;
        s.write(BASE + 0xA, 2, 0x0003, now, &mut irq);
        assert_eq!(xfer(&mut s, &mut irq, &mut now, 0x01), (0xFF, true));
        assert_eq!(xfer(&mut s, &mut irq, &mut now, 0x43), (0x41, false));
        assert_eq!(xfer(&mut s, &mut irq, &mut now, 0x00), (0xFF, false));
    }

    #[test]
    fn nothing_connected_or_slot_two() {
        let mut s = Sio0::default();
        let mut irq = Irq::default();
        s.pads[0].buttons = Some([0xFF, 0xFF]);
        let mut now = 0;
        s.write(BASE + 0xA, 2, 0x2003, now, &mut irq);
        assert_eq!(xfer(&mut s, &mut irq, &mut now, 0x01), (0xFF, false));
    }

    #[test]
    fn transfer_takes_eight_bit_times() {
        let mut s = Sio0::default();
        let mut irq = Irq::default();
        s.baud = 0x88;
        s.mode = 0x0D;
        s.write(BASE + 0xA, 2, 0x0003, 0, &mut irq);
        s.write(BASE, 1, 1, 100, &mut irq);
        assert_eq!(s.read(BASE + 4, 100 + 8 * 0x88 - 1, &mut irq) & 3, 0);
        assert_eq!(s.read(BASE + 4, 100 + 8 * 0x88, &mut irq) & 3, 3);
    }
}
