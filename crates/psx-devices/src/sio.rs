//! SIO0, the controller and memory card port (psx-spx "Controllers and
//! Memory Cards"), at register level: JOY_DATA/STAT/MODE/CTRL/BAUD at
//! 1F801040h-1F80104Eh. Used when original SDK code (libpad, libmcx, the
//! BIOS pad driver) runs against devices, as in conformance runs; with
//! adapters the pad state goes straight to the SDK's buffers.
//!
//! A bit-level port of the emulator oracle's model (Beetle PSX
//! `frontio.c`, `FrontIO_*`, `InputDevice_Gamepad_*` and
//! `InputDevice_Memcard_*`), so original code sees the same timing:
//! - transmit and receive each start when pending and enabled (a write to
//!   JOY_DATA makes a transmit pending; a read makes a receive pending), and
//!   both are clocked by one divider of `max(0x20, baud << shift) & ~1`
//!   cycles per bit;
//! - every tick clocks all four slot devices (two pads, two cards) with the
//!   transmitted bit; the received bit is their AND;
//! - a device that wants to answer pulses /ACK after its byte (a pad 0x40
//!   cycles after, a card 0x100; cancelled if it is clocked again first),
//!   active for 32 cycles; the pulse raises IRQ7 when JOY_CTRL bit 12 is
//!   set.
//!
//! The cards are the BIOS's images (`psx-bios::card`, D43), which the
//! caller passes to every access; each answers as
//! [`crate::memcard::Responder`] says.
//!
//! State advances lazily, event by event in time order, to the cycle of
//! each register access.

use crate::irq::{self, Irq};
use crate::memcard::{Card, Responder};

pub const BASE: u32 = 0x1F80_1040;
const SCALE_SHIFT: [u32; 4] = [0, 0, 4, 6];
const ACK_DELAY: i64 = 0x40;
/// A memory card's /ACK comes later than a pad's (Beetle's
/// `InputDevice_Memcard`).
pub const CARD_ACK_DELAY: i64 = 0x100;
const ACK_LEN: i64 = 32;

/// The memory cards in the two slots, as the caller keeps them.
pub type Cards = [Option<Card>; 2];

/// A memory card slot's end of the wire: the bits of the byte coming in
/// and of the one the card is sending.
#[derive(Debug, Clone, Default)]
struct CardPort {
    dtr: bool,
    bitpos: u32,
    rx: u8,
    sending: Option<u8>,
    card: Responder,
}

impl CardPort {
    fn set_dtr(&mut self, dtr: bool) {
        if !self.dtr && dtr {
            self.bitpos = 0;
            self.sending = None;
            self.card.select();
        }
        self.dtr = dtr;
    }

    /// One bit each way; sets `ack_delay` (0 = none).
    fn clock(&mut self, card: Option<&mut Card>, txd: bool, ack_delay: &mut i64) -> bool {
        *ack_delay = 0;
        if !self.dtr {
            return true;
        }
        let ret = self.sending.is_none_or(|b| (b >> self.bitpos) & 1 != 0);
        self.rx &= !(1 << self.bitpos);
        self.rx |= (txd as u8) << self.bitpos;
        self.bitpos = (self.bitpos + 1) & 7;
        if self.bitpos == 0 {
            self.sending = self.card.byte(card, self.rx);
            if self.sending.is_some() {
                *ack_delay = CARD_ACK_DELAY;
            }
        }
        ret
    }
}

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
    /// The memory card slots.
    cards: [CardPort; 2],
    /// Per slot device (pad 1, pad 2, card 1, card 2): cycles to the /ACK
    /// pulse, and the cycle the pulse ends.
    ack_delay: [i64; 4],
    ack_until: [i64; 4],
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
    pub fn update(&mut self, now: u64, irq: &mut Irq, cards: &mut Cards) {
        let now = now as i64;
        while let Some(t) = self.next_event().filter(|&t| t <= now) {
            self.step(t, irq, cards);
        }
        self.step(now, irq, cards);
    }

    /// Beetle's `FrontIO_Update` at `ts`.
    fn step(&mut self, ts: i64, irq: &mut Irq, cards: &mut Cards) {
        let clocks = ts - self.last;
        for i in 0..4 {
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
                let (pad_acks, card_acks) = self.ack_delay.split_at_mut(2);
                for (p, delay) in self.pads.iter_mut().zip(pad_acks) {
                    rxd &= p.clock(txd, delay);
                }
                for ((c, card), delay) in self.cards.iter_mut().zip(cards.iter_mut()).zip(card_acks) {
                    rxd &= c.clock(card.as_mut(), txd, delay);
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

    pub fn read(&mut self, addr: u32, now: u64, irq: &mut Irq, cards: &mut Cards) -> u32 {
        self.update(now, irq, cards);
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

    pub fn write(&mut self, addr: u32, size: u32, v: u32, now: u64, irq: &mut Irq, cards: &mut Cards) {
        self.update(now, irq, cards);
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
            self.cards[s].set_dtr(dtr);
            if !dtr {
                for d in [s, 2 + s] {
                    self.ack_delay[d] = 0;
                    self.ack_until[d] = -1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The port, its interrupt controller and the slots' cards.
    struct Port {
        s: Sio0,
        irq: Irq,
        cards: Cards,
        now: u64,
    }

    impl Port {
        fn read(&mut self, addr: u32, now: u64) -> u32 {
            self.s.read(addr, now, &mut self.irq, &mut self.cards)
        }

        fn write(&mut self, addr: u32, size: u32, v: u32, now: u64) {
            self.s.write(addr, size, v, now, &mut self.irq, &mut self.cards)
        }

        /// One exchange as libpad does it: write TX, wait for RX, read it,
        /// note /ACK, acknowledge.
        fn xfer(&mut self, b: u8) -> (u8, bool) {
            self.write(BASE, 1, b as u32, self.now);
            let mut ack = false;
            for _ in 0..4000 {
                self.now += 4;
                ack |= self.read(BASE + 4, self.now) & 0x80 != 0;
                if self.read(BASE + 4, self.now) & 2 != 0 {
                    break;
                }
            }
            let rx = self.read(BASE, self.now) as u8;
            for _ in 0..200 {
                self.now += 4;
                ack |= self.read(BASE + 4, self.now) & 0x80 != 0;
            }
            let ctrl = self.s.ctrl as u32 | 0x10;
            self.write(BASE + 0xA, 2, ctrl, self.now);
            (rx, ack)
        }
    }

    fn port(ctrl: u32, buttons: Option<[u8; 2]>) -> Port {
        let mut p = Port { s: Sio0::default(), irq: Irq::default(), cards: [None, None], now: 2 };
        p.s.pads[0].buttons = buttons;
        p.write(BASE + 0xE, 2, 0x88, 0);
        p.write(BASE + 8, 2, 0x0D, 0);
        p.write(BASE + 0xA, 2, ctrl, 0);
        // Arm the receiver as pad drivers do (a read): with RXEN clear it
        // starts together with the next transmit.
        p.read(BASE, 1);
        p
    }

    #[test]
    fn digital_pad_poll() {
        let mut p = port(0x1003, Some([0xF7, 0xFF]));
        let got: Vec<(u8, bool)> = [0x01, 0x42, 0x00, 0x00, 0x00].iter().map(|&b| p.xfer(b)).collect();
        assert_eq!(got, [(0xFF, true), (0x41, true), (0x5A, true), (0xF7, true), (0xFF, false)]);
        assert_ne!(p.irq.stat & 1 << irq::SIO0, 0, "/ACK raised IRQ7");
    }

    #[test]
    fn unknown_command_ends_the_exchange() {
        let mut p = port(0x0003, Some([0xFF, 0xFF]));
        assert_eq!(p.xfer(0x01), (0xFF, true));
        assert_eq!(p.xfer(0x43), (0x41, false));
        assert_eq!(p.xfer(0x00), (0xFF, false));
    }

    #[test]
    fn nothing_connected_or_slot_two() {
        let mut p = port(0x2003, Some([0xFF, 0xFF]));
        assert_eq!(p.xfer(0x01), (0xFF, false));
        let mut p = port(0x0003, None);
        assert_eq!(p.xfer(0x01), (0xFF, false));
    }

    /// A card answers 81h on its slot, with its FLAG and a late /ACK, and
    /// reads a frame; a pad's address leaves it silent, and an empty slot
    /// answers nothing.
    #[test]
    fn memory_card_on_its_slot() {
        let mut p = port(0x3003, Some([0xFF, 0xFF]));
        p.cards[1] = Some(Card::formatted());
        assert_eq!(p.xfer(0x81), (0xFF, true));
        assert_eq!(p.xfer(b'R'), (0x08, true));
        let got: Vec<u8> = [0, 0, 0, 1, 0, 0, 0, 0].iter().map(|&b| p.xfer(b).0).collect();
        assert_eq!(got, [0x5A, 0x5D, 0x00, 0x00, 0x5C, 0x5D, 0x00, 0x01]);
        assert_eq!(p.xfer(0).0, p.cards[1].as_ref().unwrap().frame(1)[0]);
        let mut p = port(0x3003, None);
        p.cards[1] = Some(Card::formatted());
        assert_eq!(p.xfer(0x01), (0xFF, false), "a pad's address");
        let mut p = port(0x1003, None);
        p.cards[1] = Some(Card::formatted());
        assert_eq!(p.xfer(0x81), (0xFF, false), "slot 1 is empty");
    }

    /// The card's /ACK comes 100h cycles after its byte, a pad's 40h.
    #[test]
    fn card_acknowledges_later() {
        let mut p = port(0x0003, None);
        p.cards[0] = Some(Card::formatted());
        p.write(BASE, 1, 0x81, 20_000);
        let end = 20_000 + 8 * 0x88;
        assert_eq!(p.read(BASE + 4, end + CARD_ACK_DELAY as u64 - 1) & 0x80, 0);
        assert_eq!(p.read(BASE + 4, end + CARD_ACK_DELAY as u64) & 0x80, 0x80);
    }

    #[test]
    fn transfer_takes_eight_bit_times() {
        let mut p = port(0x0003, Some([0xFF, 0xFF]));
        p.write(BASE, 1, 1, 20_000);
        assert_eq!(p.read(BASE + 4, 20_000 + 8 * 0x88 - 1) & 2, 0);
        assert_eq!(p.read(BASE + 4, 20_000 + 8 * 0x88) & 2, 2);
    }

    #[test]
    fn receive_needs_a_read_first() {
        // With RXEN (bit 2) a read starts a receive at once, clocking the
        // pad with zero bits; without a read, nothing is received.
        let mut p = Port { s: Sio0::default(), irq: Irq::default(), cards: [None, None], now: 0 };
        p.s.pads[0].buttons = Some([0xFF, 0xFF]);
        p.write(BASE + 0xA, 2, 0x0007, 0);
        p.write(BASE, 1, 1, 0);
        assert_eq!(p.read(BASE + 4, 10_000) & 3, 1, "sent, nothing received");
    }
}
