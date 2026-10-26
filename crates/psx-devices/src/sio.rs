//! SIO0, the controller and memory card port (psx-spx "Controllers and
//! Memory Cards"), at register level: JOY_DATA/STAT/MODE/CTRL/BAUD at
//! 1F801040h-1F80104Eh. Used when original SDK code (libpad, the BIOS pad
//! driver) runs against devices, as in conformance runs; with adapters
//! the pad state goes straight to the SDK's buffers.
//!
//! A bit-level port of the emulator oracle's model (Beetle PSX
//! `frontio.c`, `FrontIO_*` and `InputDevice_Gamepad_*`), so original code
//! sees the same timing:
//! - transmit and receive each start when pending and enabled (a write to
//!   JOY_DATA makes a transmit pending; a read makes a receive pending), and
//!   both are clocked by one divider of `max(0x20, baud << shift) & ~1`
//!   cycles per bit;
//! - every tick clocks all four slot devices (two pads, two cards) with the
//!   transmitted bit; the received bit is their AND;
//! - a device that wants to answer pulses /ACK 0x40 cycles after its byte
//!   (cancelled if it is clocked again first), active for 32 cycles; the
//!   pulse raises IRQ7 when JOY_CTRL bit 12 is set.
//!
//! State advances lazily, event by event in time order, to the cycle of
//! each register access. Memory cards are absent (they never answer).

use crate::irq::{self, Irq};

pub const BASE: u32 = 0x1F80_1040;
const SCALE_SHIFT: [u32; 4] = [0, 0, 4, 6];
const ACK_DELAY: i64 = 0x40;
const ACK_LEN: i64 = 32;

/// A digital pad (Beetle's `InputDevice_Gamepad`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pad {
    /// The two button bytes as sent on the wire (active low); `None`:
    /// nothing connected.
    pub buttons: Option<[u8; 2]>,
    dtr: bool,
    phase: i8,
    bitpos: u32,
    rx: u8,
    tx: [u8; 3],
    tpos: usize,
    tcount: usize,
}

impl Pad {
    fn set_dtr(&mut self, dtr: bool) {
        if !self.dtr && dtr {
            self.phase = 0;
            self.bitpos = 0;
            self.tpos = 0;
            self.tcount = 0;
        }
        self.dtr = dtr;
    }

    /// One bit each way; sets `ack_delay` (0 = none).
    fn clock(&mut self, txd: bool, ack_delay: &mut i64) -> bool {
        *ack_delay = 0;
        let Some(buttons) = self.buttons else { return true };
        if !self.dtr {
            return true;
        }
        let ret = if self.tcount > 0 { (self.tx[self.tpos] >> self.bitpos) & 1 != 0 } else { true };
        self.rx &= !(1 << self.bitpos);
        self.rx |= (txd as u8) << self.bitpos;
        self.bitpos = (self.bitpos + 1) & 7;
        if self.bitpos == 0 {
            if self.tcount > 0 {
                self.tpos += 1;
                self.tcount -= 1;
            }
            match self.phase {
                0 if self.rx != 0x01 => self.phase = -1,
                0 => {
                    self.tx[0] = 0x41;
                    self.tpos = 0;
                    self.tcount = 1;
                    self.phase = 1;
                }
                1 => {
                    self.phase = 2;
                    self.tx[0] = 0x5A;
                    if self.rx == 0x42 {
                        self.tx[1] = buttons[0];
                        self.tx[2] = buttons[1];
                        self.tpos = 0;
                        self.tcount = 3;
                    } else {
                        self.phase = -1;
                        self.tx[1] = 0;
                        self.tx[2] = 0;
                        self.tpos = 0;
                        self.tcount = 0;
                    }
                }
                _ => {}
            }
        }
        if self.bitpos == 0 && self.tcount > 0 {
            *ack_delay = ACK_DELAY;
        }
        ret
    }
}

#[derive(Debug, Clone, Default)]
pub struct Sio0 {
    pub mode: u16,
    pub ctrl: u16,
    pub baud: u16,
    pub pads: [Pad; 2],
    tx_byte: u8,
    tx_pending: bool,
    tx_busy: bool,
    tx_bits: u32,
    rx_byte: u8,
    rx_pending: bool,
    rx_busy: bool,
    rx_bits: u32,
    rx_avail: bool,
    /// Cycles to the next bit tick (0: idle).
    divider: i64,
    /// Per slot device (pad 1, pad 2): cycles to the /ACK pulse, and the
    /// cycle the pulse ends.
    ack_delay: [i64; 2],
    ack_until: [i64; 2],
    istatus: bool,
    last: i64,
    /// Bytes the host sent to the selected pad and its replies, for
    /// decoders: (slot, host byte, reply).
    pub log: Vec<(u8, u8, u8)>,
}

impl Sio0 {
    pub fn handles(addr: u32) -> bool {
        (BASE..BASE + 0x10).contains(&(addr | 0x1F00_0000))
    }

    fn period(&self) -> i64 {
        (((self.baud as i64) << SCALE_SHIFT[(self.mode & 3) as usize]) & !1).max(0x20)
    }

    fn next_event(&self) -> Option<i64> {
        let mut next: Option<i64> = None;
        let mut take = |d: i64| {
            if d > 0 {
                next = Some(next.map_or(d, |n| n.min(d)));
            }
        };
        take(self.divider);
        self.ack_delay.iter().for_each(|&d| take(d));
        next.map(|d| self.last + d)
    }

    /// Bring the port up to cycle `now`, processing every event before it
    /// at its own time (as the oracle's scheduler does).
    pub fn update(&mut self, now: u64, irq: &mut Irq) {
        let now = now as i64;
        while let Some(t) = self.next_event().filter(|&t| t <= now) {
            self.step(t, irq);
        }
        self.step(now, irq);
    }

    /// Beetle's `FrontIO_Update` at `ts`.
    fn step(&mut self, ts: i64, irq: &mut Irq) {
        let clocks = ts - self.last;
        for i in 0..2 {
            if self.ack_delay[i] > 0 {
                self.ack_delay[i] -= clocks;
                if self.ack_delay[i] <= 0 {
                    self.ack_until[i] = ts + ACK_LEN + self.ack_delay[i];
                    if self.ctrl & 0x1000 != 0 {
                        self.raise(irq);
                    }
                }
            }
        }
        let mut check = false;
        if self.divider > 0 {
            self.divider -= clocks;
            while self.divider <= 0 {
                if !(self.rx_busy || self.tx_busy) {
                    break;
                }
                let mut txd = false;
                if self.tx_busy {
                    txd = (self.tx_byte >> self.tx_bits) & 1 != 0;
                    self.tx_bits = (self.tx_bits + 1) & 7;
                    if self.tx_bits == 0 {
                        check = true;
                        self.tx_busy = false;
                        if self.ctrl & 0x400 != 0 {
                            self.raise(irq);
                        }
                    }
                }
                let mut rxd = true;
                for (p, delay) in self.pads.iter_mut().zip(self.ack_delay.iter_mut()) {
                    rxd &= p.clock(txd, delay);
                }
                if self.rx_busy {
                    self.rx_byte &= !(1 << self.rx_bits);
                    self.rx_byte |= (rxd as u8) << self.rx_bits;
                    self.rx_bits = (self.rx_bits + 1) & 7;
                    if self.rx_bits == 0 {
                        check = true;
                        self.rx_busy = false;
                        self.rx_avail = true;
                        if let Some(s) = self.selected() {
                            self.log.push((s as u8, self.tx_byte, self.rx_byte));
                        }
                        if self.ctrl & 0x800 != 0 {
                            self.raise(irq);
                        }
                    }
                }
                self.divider += self.period();
            }
        }
        self.last = ts;
        if check {
            self.start_stop();
        }
    }

    fn selected(&self) -> Option<usize> {
        (self.ctrl & 2 != 0).then_some(((self.ctrl >> 13) & 1) as usize)
    }

    /// Beetle's `FrontIO_CheckStartStopPending`.
    fn start_stop(&mut self) {
        if (self.rx_pending && self.ctrl & 4 != 0) || (self.tx_pending && self.ctrl & 1 != 0) {
            if self.rx_pending {
                self.rx_pending = false;
                self.rx_busy = true;
                self.rx_avail = false;
                self.rx_byte = 0;
                self.rx_bits = 0;
            }
            if self.tx_pending {
                self.tx_pending = false;
                self.tx_busy = true;
                self.tx_bits = 0;
            }
            self.divider = self.period();
        }
        if self.ctrl & 5 == 0 {
            self.rx_busy = false;
            self.tx_busy = false;
        }
        if !self.rx_busy && !self.tx_busy {
            self.divider = 0;
        }
    }

    fn raise(&mut self, irq: &mut Irq) {
        self.istatus = true;
        irq.raise(irq::SIO0);
    }

    pub fn read(&mut self, addr: u32, now: u64, irq: &mut Irq) -> u32 {
        self.update(now, irq);
        let reg = (addr | 0x1F00_0000) - BASE;
        let v = match reg & !1 {
            0 | 2 => {
                let b = self.rx_byte as u32;
                self.rx_avail = false;
                self.rx_pending = true;
                self.rx_busy = false;
                self.start_stop();
                return b * 0x0101_0101 >> (8 * (reg & 3));
            }
            4 | 6 => {
                let mut v = 0;
                if !self.tx_pending && !self.tx_busy {
                    v |= 1;
                }
                if self.rx_avail {
                    v |= 2;
                }
                if self.ack_until.iter().any(|&u| (now as i64) < u) {
                    v |= 0x80;
                }
                if self.istatus {
                    v |= 0x200;
                }
                v
            }
            8 => self.mode as u32,
            0xA => self.ctrl as u32,
            0xE => self.baud as u32,
            _ => 0,
        };
        v >> (8 * (reg & 1)) >> if reg & 2 != 0 && reg < 8 { 16 } else { 0 }
    }

    pub fn write(&mut self, addr: u32, size: u32, v: u32, now: u64, irq: &mut Irq) {
        self.update(now, irq);
        match (addr | 0x1F00_0000) - BASE {
            0 => {
                self.tx_byte = v as u8;
                self.tx_pending = true;
                self.tx_busy = false;
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
        self.start_stop();
    }

    fn write_ctrl(&mut self, v: u16) {
        self.ctrl = v & 0x3F2F;
        // Acknowledge and reset lower the IRQ line; I_STAT keeps the edge
        // it latched until software clears it there.
        if v & 0x10 != 0 {
            self.istatus = false;
        }
        if v & 0x40 != 0 {
            self.istatus = false;
            self.divider = 0;
            self.rx_pending = false;
            self.tx_pending = false;
            self.rx_busy = false;
            self.tx_busy = false;
            self.rx_avail = false;
            self.tx_byte = 0;
            self.rx_byte = 0;
            self.rx_bits = 0;
            self.tx_bits = 0;
            self.mode = 0;
            self.ctrl = 0;
            self.baud = 0;
        }
        for s in 0..2 {
            let dtr = self.ctrl & 2 != 0 && ((self.ctrl >> 13) & 1) as usize == s;
            self.pads[s].set_dtr(dtr);
            if !dtr {
                self.ack_delay[s] = 0;
                self.ack_until[s] = -1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One exchange as libpad does it: write TX, wait for RX, read it,
    /// note /ACK, acknowledge.
    fn xfer(s: &mut Sio0, irq: &mut Irq, now: &mut u64, b: u8) -> (u8, bool) {
        s.write(BASE, 1, b as u32, *now, irq);
        let mut ack = false;
        for _ in 0..4000 {
            *now += 4;
            ack |= s.read(BASE + 4, *now, irq) & 0x80 != 0;
            if s.read(BASE + 4, *now, irq) & 2 != 0 {
                break;
            }
        }
        let rx = s.read(BASE, *now, irq) as u8;
        for _ in 0..100 {
            *now += 4;
            ack |= s.read(BASE + 4, *now, irq) & 0x80 != 0;
        }
        s.write(BASE + 0xA, 2, s.ctrl as u32 | 0x10, *now, irq);
        (rx, ack)
    }

    fn port(ctrl: u32, buttons: Option<[u8; 2]>) -> (Sio0, Irq, u64) {
        let mut s = Sio0::default();
        let mut irq = Irq::default();
        s.pads[0].buttons = buttons;
        s.write(BASE + 0xE, 2, 0x88, 0, &mut irq);
        s.write(BASE + 8, 2, 0x0D, 0, &mut irq);
        s.write(BASE + 0xA, 2, ctrl, 0, &mut irq);
        // Arm the receiver as pad drivers do (a read): with RXEN clear it
        // starts together with the next transmit.
        s.read(BASE, 1, &mut irq);
        (s, irq, 2)
    }

    #[test]
    fn digital_pad_poll() {
        let (mut s, mut irq, mut now) = port(0x1003, Some([0xF7, 0xFF]));
        let got: Vec<(u8, bool)> = [0x01, 0x42, 0x00, 0x00, 0x00].iter().map(|&b| xfer(&mut s, &mut irq, &mut now, b)).collect();
        assert_eq!(got, [(0xFF, true), (0x41, true), (0x5A, true), (0xF7, true), (0xFF, false)]);
        assert_ne!(irq.stat & 1 << irq::SIO0, 0, "/ACK raised IRQ7");
    }

    #[test]
    fn unknown_command_ends_the_exchange() {
        let (mut s, mut irq, mut now) = port(0x0003, Some([0xFF, 0xFF]));
        assert_eq!(xfer(&mut s, &mut irq, &mut now, 0x01), (0xFF, true));
        assert_eq!(xfer(&mut s, &mut irq, &mut now, 0x43), (0x41, false));
        assert_eq!(xfer(&mut s, &mut irq, &mut now, 0x00), (0xFF, false));
    }

    #[test]
    fn nothing_connected_or_slot_two() {
        let (mut s, mut irq, mut now) = port(0x2003, Some([0xFF, 0xFF]));
        assert_eq!(xfer(&mut s, &mut irq, &mut now, 0x01), (0xFF, false));
        let (mut s, mut irq, mut now) = port(0x0003, None);
        assert_eq!(xfer(&mut s, &mut irq, &mut now, 0x01), (0xFF, false));
    }

    #[test]
    fn transfer_takes_eight_bit_times() {
        let (mut s, mut irq, _) = port(0x0003, Some([0xFF, 0xFF]));
        s.write(BASE, 1, 1, 20_000, &mut irq);
        assert_eq!(s.read(BASE + 4, 20_000 + 8 * 0x88 - 1, &mut irq) & 2, 0);
        assert_eq!(s.read(BASE + 4, 20_000 + 8 * 0x88, &mut irq) & 2, 2);
    }

    #[test]
    fn receive_needs_a_read_first() {
        // With RXEN (bit 2) a read starts a receive at once, clocking the
        // pad with zero bits; without a read, nothing is received.
        let mut s = Sio0::default();
        let mut irq = Irq::default();
        s.pads[0].buttons = Some([0xFF, 0xFF]);
        s.write(BASE + 0xA, 2, 0x0007, 0, &mut irq);
        s.write(BASE, 1, 1, 0, &mut irq);
        assert_eq!(s.read(BASE + 4, 10_000, &mut irq) & 3, 1, "sent, nothing received");
    }
}
