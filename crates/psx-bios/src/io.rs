//! Register-level I/O for device libraries (D40, D42) and conformance
//! runs: guest accesses to the I/O window reach the interrupt controller,
//! SIO0, the root counters, the DMA controller and the GPU here, and
//! interrupts run the kernel's `SysEnqIntRP` chains as the BIOS exception
//! handler does. Without a bus, an I/O access is a runtime trap: the
//! function needs an adapter.
//!
//! DMA transfers complete instantly in virtual time. The GPU front end's
//! output goes to the host renderer at each vblank, and whenever a
//! VRAM→CPU transfer needs pixels.
//!
//! Chain walking follows OpenBIOS (`kernel/vectors.s`, the oracle's BIOS):
//! priorities 0 to 3, newest element first; an element's verifier
//! (+08h) runs if set, and when it returns nonzero its handler (+04h) runs
//! with that value in `a0`.

use crate::machine::{GuestCall, IrqStep, Machine, Sdk};
use psx_devices::dma::{self, Dma};
use psx_devices::gpu::Gpu;
use psx_devices::irq::{self, Irq};
use psx_devices::sio::Sio0;
use std::collections::BTreeMap;

const TIMERS: std::ops::Range<u32> = 0x1F80_1100..0x1F80_1130;
const GP0: u32 = 0x1F80_1810;
const GP1: u32 = 0x1F80_1814;
/// Linked-list nodes one GPU DMA may walk before it is called a loop.
const MAX_NODES: u32 = 1 << 20;
/// Memory control (expansion bases and delays, RAM size): kept, no effect.
const MEMCTRL: [std::ops::Range<u32>; 2] = [0x1F80_1000..0x1F80_1024, 0x1F80_1060..0x1F80_1064];

#[derive(Debug, Clone, Default)]
pub struct Io {
    pub irq: Irq,
    pub sio0: Sio0,
    pub dma: Dma,
    pub gpu: Gpu,
    /// GPU words received (GP0 port and DMA), for the run report.
    pub gp0_words: u64,
    /// Memory-control words written, by address.
    pub memctrl: BTreeMap<u32, u32>,
    /// The last [`RECENT`] accesses: (cycle, address, size, value, write).
    pub recent: std::collections::VecDeque<(u64, u32, u32, u32, bool)>,
}

pub const RECENT: usize = 256;

impl Io {
    fn note(&mut self, now: u64, addr: u32, size: u32, v: u32, write: bool) {
        if self.recent.len() == RECENT {
            self.recent.pop_front();
        }
        self.recent.push_back((now, addr, size, v, write));
    }
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
        } else if Dma::handles(addr) {
            io.dma.read(addr)
        } else if addr & !3 == GP0 {
            io.gpu.read() >> (8 * (addr & 3))
        } else if addr & !3 == GP1 {
            io.gpu.status() >> (8 * (addr & 3))
        } else {
            return Err(format!("read of unmodelled I/O register {addr:#010x} ({size} bytes)"));
        };
        let v = v & mask(size);
        if let Some(io) = self.io.as_mut() {
            io.note(now, addr, size, v, false);
        }
        Ok(v)
    }

    /// Write `size` bytes of `v` to the register at `addr` (physical).
    pub fn io_write(&mut self, addr: u32, size: u32, v: u32) -> Result<(), String> {
        let now = self.clock.now;
        let video = self.clock.video;
        let Some(io) = self.io.as_mut() else { return Err(no_bus(addr)) };
        io.sio0.update(now, &mut io.irq);
        let v = v & mask(size);
        io.note(now, addr, size, v, true);
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
        } else if Dma::handles(addr) {
            if io.dma.write(addr, size, v).is_some() {
                self.run_dma()?;
            }
        } else if addr & !3 == GP0 {
            self.gp0(v);
        } else if addr & !3 == GP1 {
            io.gpu.gp1(v);
        } else {
            return Err(format!("write of {v:#x} to unmodelled I/O register {addr:#010x} ({size} bytes)"));
        }
        Ok(())
    }

    /// One GP0 word; a VRAM→CPU transfer it starts gets its pixels from the
    /// host renderer at once.
    pub fn gp0(&mut self, word: u32) {
        let Some(io) = self.io.as_mut() else { return };
        io.gpu.gp0(word);
        io.gp0_words += 1;
        if let Some(r) = io.gpu.pending_readback() {
            let words = io.gpu.take_commands();
            if !words.is_empty() {
                self.host.render_submit(&words);
            }
            let px = self.host.render_readback(r.x, r.y, r.w, r.h);
            if let Some(io) = self.io.as_mut() {
                io.gpu.finish_readback(Some(&px));
            }
        }
    }

    /// Run every DMA channel that is ready.
    fn run_dma(&mut self) -> Result<(), String> {
        while let Some(ch) = self.io.as_ref().and_then(|io| (0..7).find(|&c| io.dma.ready(c))) {
            self.dma_transfer(ch)?;
            let raise = self.io.as_mut().is_some_and(|io| io.dma.complete(ch));
            S::on_dma(self, ch);
            if raise {
                self.raise_irq(irq::DMA);
            }
        }
        Ok(())
    }

    fn dma_transfer(&mut self, ch: usize) -> Result<(), String> {
        let Some(io) = self.io.as_ref() else { return Ok(()) };
        let d = io.dma;
        let c = d.chcr[ch];
        let step: u32 = if c & dma::CHCR_STEP_BACK != 0 { 4u32.wrapping_neg() } else { 4 };
        let mut addr = d.madr[ch] & 0x1F_FFFC;
        match (ch, d.sync_mode(ch), c & dma::CHCR_FROM_RAM != 0) {
            (dma::OTC, _, _) => {
                let n = d.words(ch);
                for i in 0..n {
                    let v = if i + 1 == n { 0x00FF_FFFF } else { addr.wrapping_sub(4) & 0x1F_FFFF };
                    self.mem.w32(addr, v);
                    addr = addr.wrapping_sub(4) & 0x1F_FFFC;
                }
            }
            (dma::GPU, 2, true) => {
                let mut nodes = 0;
                loop {
                    let header = self.mem.r32(addr);
                    for i in 1..=(header >> 24) {
                        let w = self.mem.r32(addr.wrapping_add(4 * i) & 0x1F_FFFC);
                        self.gp0(w);
                    }
                    nodes += 1;
                    if header & 0x80_0000 != 0 {
                        break;
                    }
                    if nodes == MAX_NODES {
                        return Err(format!("GPU DMA linked list at {:#010x} does not end", d.madr[ch]));
                    }
                    addr = header & 0x1F_FFFC;
                }
                if let Some(io) = self.io.as_mut() {
                    io.dma.madr[ch] = 0x00FF_FFFF;
                }
                return Ok(());
            }
            (dma::GPU, 0 | 1, true) => {
                for _ in 0..d.words(ch) {
                    let w = self.mem.r32(addr);
                    self.gp0(w);
                    addr = addr.wrapping_add(step) & 0x1F_FFFC;
                }
            }
            (dma::GPU, 0 | 1, false) => {
                for _ in 0..d.words(ch) {
                    let w = self.io.as_mut().map_or(0, |io| io.gpu.read());
                    self.mem.w32(addr, w);
                    addr = addr.wrapping_add(step) & 0x1F_FFFC;
                }
            }
            (ch, mode, from_ram) => {
                return Err(format!("DMA channel {ch} (sync mode {mode}, from RAM {from_ram}) is not modelled"));
            }
        }
        if let Some(io) = self.io.as_mut() {
            io.dma.madr[ch] = addr;
            if d.sync_mode(ch) == 1 {
                io.dma.bcr[ch] &= 0xFFFF;
            }
        }
        Ok(())
    }

    /// At each vblank: the frame so far to the renderer, and the display.
    pub fn gpu_vblank(&mut self) {
        let odd = self.vblanks & 1 == 1;
        let Some(io) = self.io.as_mut() else { return };
        io.gpu.set_field(odd);
        let words = io.gpu.take_commands();
        let desc = io.gpu.present_desc();
        if !words.is_empty() {
            self.host.render_submit(&words);
        }
        self.host.present(&desc);
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

    /// The BIOS exception handler's walk of the `SysEnqIntRP` chains,
    /// then its exit.
    pub fn queue_irq_chains(&mut self) {
        for prio in 0..4u32 {
            let Some(chain) = self.kernel.int_handlers.get(&prio) else { continue };
            for &h in chain.iter().rev() {
                let verifier = self.mem.r32(h + 8);
                let handler = self.mem.r32(h + 4);
                if verifier != 0 {
                    self.irq_calls.push_back(IrqStep::Chain(GuestCall { addr: verifier, args: [0; 4] }, (handler != 0).then_some(handler)));
                }
            }
        }
        self.irq_calls.push_back(IrqStep::Exit);
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
    use crate::machine::{GuestCall, IrqStep};
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
        assert_eq!(m.irq_calls.pop_front(), Some(IrqStep::Chain(GuestCall { addr: 0x8000_2000, args: [0; 4] }, Some(0x8000_3000))));
        assert_eq!(m.irq_calls.pop_front(), Some(IrqStep::Exit));
        m.io_write(0x1F80_1070, 4, !1).unwrap();
        assert_eq!(m.io_read(0x1F80_1070, 2).unwrap(), 0);
        assert!(m.io_read(0x1F80_1C00, 2).is_err(), "the SPU is not on the bus");
        // GP1(08h) display mode, read back in GPUSTAT bits 17-22.
        m.io_write(0x1F80_1814, 4, 0x0800_0001).unwrap();
        assert_eq!((m.io_read(0x1F80_1814, 4).unwrap() >> 17) & 3, 1);
        m.io_write(0x1F80_1124, 2, 0x0200).unwrap();
        assert_eq!(m.io_read(0x1F80_1124, 2).unwrap(), 0x0600);
    }
}
