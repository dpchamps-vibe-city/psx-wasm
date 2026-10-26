//! Register-level I/O for original SDK code (conformance runs): when the
//! machine has an [`Io`] bus, guest accesses to the I/O window reach the
//! interrupt controller, SIO0 and the root counters here, and interrupts
//! run the kernel's `SysEnqIntRP` chains as the BIOS exception handler
//! does. Without it (the default), an I/O access is a runtime trap: the
//! function needs an adapter.
//!
//! Chain walking follows OpenBIOS (`kernel/vectors.s`, the oracle's BIOS):
//! priorities 0 to 3, newest element first; an element's verifier
//! (+08h) runs if set, and when it returns nonzero its handler (+04h) runs
//! with that value in `a0`.

use crate::machine::{GuestCall, Machine, Sdk};
use psx_devices::irq::{self, Irq};
use psx_devices::sio::Sio0;
use std::collections::BTreeMap;

const TIMERS: std::ops::Range<u32> = 0x1F80_1100..0x1F80_1130;
/// Memory control (expansion bases and delays, RAM size): kept, no effect.
const MEMCTRL: [std::ops::Range<u32>; 2] = [0x1F80_1000..0x1F80_1024, 0x1F80_1060..0x1F80_1064];

#[derive(Debug, Clone, Default)]
pub struct Io {
    pub irq: Irq,
    pub sio0: Sio0,
    /// Memory-control words written, by address.
    pub memctrl: BTreeMap<u32, u32>,
}

impl<S: Sdk> Machine<S> {
    /// Read `size` bytes of the register at `addr` (physical).
    pub fn io_read(&mut self, addr: u32, size: u32) -> Result<u32, String> {
        let now = self.clock.now;
        let video = self.clock.video;
        let Some(io) = self.io.as_mut() else { return Err(no_bus(addr)) };
        io.sio0.update(now, &mut io.irq);
        let v = if Irq::handles(addr) {
            io.irq.read(addr)
        } else if Sio0::handles(addr) {
            io.sio0.read(addr, now, &mut io.irq)
        } else if TIMERS.contains(&addr) {
            let n = ((addr - TIMERS.start) >> 4) as usize;
            let t = &self.timers[n];
            let reg = match (addr >> 2) & 3 {
                0 => t.value(n, video, now) as u32,
                1 => t.mode as u32 | 0x400,
                2 => t.target as u32,
                _ => 0,
            };
            reg >> (8 * (addr & 3))
        } else if MEMCTRL.iter().any(|r| r.contains(&addr)) {
            io.memctrl.get(&(addr & !3)).copied().unwrap_or(0) >> (8 * (addr & 3))
        } else {
            return Err(format!("read of unmodelled I/O register {addr:#010x} ({size} bytes)"));
        };
        Ok(v & mask(size))
    }

    /// Write `size` bytes of `v` to the register at `addr` (physical).
    pub fn io_write(&mut self, addr: u32, size: u32, v: u32) -> Result<(), String> {
        let now = self.clock.now;
        let video = self.clock.video;
        let Some(io) = self.io.as_mut() else { return Err(no_bus(addr)) };
        io.sio0.update(now, &mut io.irq);
        let v = v & mask(size);
        if Irq::handles(addr) {
            io.irq.write(addr, size, v);
        } else if Sio0::handles(addr) {
            io.sio0.write(addr, size, v, now, &mut io.irq);
        } else if TIMERS.contains(&addr) {
            let n = ((addr - TIMERS.start) >> 4) as usize;
            match (addr >> 2) & 3 {
                0 => self.timers[n].write_count(n, video, now, v as u16),
                1 => self.timers[n].write_mode(v as u16, now),
                2 => self.timers[n].target = v as u16,
                _ => {}
            }
            self.reschedule_timer(n);
        } else if MEMCTRL.iter().any(|r| r.contains(&addr)) {
            io.memctrl.insert(addr & !3, v << (8 * (addr & 3)));
        } else {
            return Err(format!("write of {v:#x} to unmodelled I/O register {addr:#010x} ({size} bytes)"));
        }
        Ok(())
    }

    /// Raise interrupt `bit` on the bus (if there is one) and, when it is
    /// unmasked, queue the kernel's handler chains.
    pub fn raise_irq(&mut self, bit: u32) {
        let pads: Vec<Vec<u8>> = if bit == irq::VBLANK && self.io.is_some() {
            (0..2).map(|p| self.host.pad_state(p)).collect()
        } else {
            vec![]
        };
        let Some(io) = self.io.as_mut() else { return };
        // The pad latches its buttons once per frame, as the oracle's
        // frontend does.
        for (p, st) in pads.iter().enumerate() {
            io.sio0.pads[p].buttons = (st.len() >= 2).then(|| [st[0], st[1]]);
        }
        io.irq.raise(bit);
        if io.irq.pending() && !self.crit && !self.in_irq {
            self.queue_irq_chains();
        }
    }

    /// The BIOS exception handler's walk of the `SysEnqIntRP` chains.
    pub fn queue_irq_chains(&mut self) {
        for prio in 0..4u32 {
            let Some(chain) = self.kernel.int_handlers.get(&prio) else { continue };
            for &h in chain.iter().rev() {
                let verifier = self.mem.r32(h + 8);
                let handler = self.mem.r32(h + 4);
                if verifier != 0 {
                    self.irq_calls.push_back((GuestCall { addr: verifier, args: [0; 4] }, (handler != 0).then_some(handler)));
                }
            }
        }
    }
}

fn mask(size: u32) -> u32 {
    if size >= 4 { u32::MAX } else { (1 << (8 * size)) - 1 }
}

fn no_bus(addr: u32) -> String {
    format!("guest access to I/O register {addr:#010x}; this function needs an adapter")
}

#[cfg(test)]
mod tests {
    use crate::testing::machine;

    #[test]
    fn registers_and_chains() {
        let mut m = machine();
        assert!(m.io_read(0x1F80_1070, 4).is_err(), "no bus: trap");
        m.io = Some(Default::default());
        m.io_write(0x1F80_1074, 4, 1).unwrap();
        // An element at 0x80001000: verifier 0x80002000, handler 0x80003000.
        m.mem.w32(0x8000_1004, 0x8000_3000);
        m.mem.w32(0x8000_1008, 0x8000_2000);
        m.kernel.int_handlers.entry(2).or_default().push(0x8000_1000);
        m.raise_irq(psx_devices::irq::VBLANK);
        assert_eq!(m.io_read(0x1F80_1070, 4).unwrap(), 1);
        let (call, then) = m.irq_calls.pop_front().unwrap();
        assert_eq!((call.addr, then), (0x8000_2000, Some(0x8000_3000)));
        m.io_write(0x1F80_1070, 4, !1).unwrap();
        assert_eq!(m.io_read(0x1F80_1070, 2).unwrap(), 0);
        assert!(m.io_read(0x1F80_1810, 4).is_err(), "GPU is not modelled here");
        m.io_write(0x1F80_1124, 2, 0x0200).unwrap();
        assert_eq!(m.io_read(0x1F80_1124, 2).unwrap(), 0x0600);
    }
}
