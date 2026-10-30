//! Register-level I/O for device libraries (D40, D42, D45, D47, D58) and
//! conformance runs: guest accesses to the I/O window reach the interrupt
//! controller, SIO0, the root counters, the DMA controller, the GPU, the
//! SPU, the CD controller and the MDEC here, and interrupts run the kernel's `SysEnqIntRP` chains as the
//! BIOS exception handler does. Without a bus, an I/O access is a runtime
//! trap: the function needs an adapter.
//!
//! The SPU runs on its own 44.1 kHz clock (`psx_spu::Spu::tick`, one
//! sample per 768 cycles). It is brought up to the current cycle before
//! every access to it, every DMA4 transfer and every vblank. Each tick
//! takes the next frame of the CD's output as its CD input, and the mixed
//! output goes to the host at the vblank.
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
use psx_devices::cdc::Cdc;
use psx_devices::dma::{self, Dma};
use psx_devices::gpu::Gpu;
use psx_devices::irq::{self, Irq};
use psx_devices::mdec::Mdec;
use psx_devices::sio::Sio0;
use std::collections::BTreeMap;

const TIMERS: std::ops::Range<u32> = 0x1F80_1100..0x1F80_1130;
const SPU: std::ops::Range<u32> = 0x1F80_1C00..0x1F80_2000;
/// DMA channel 4: SPU RAM transfers.
const DMA_SPU: usize = 4;
/// DMA channel 3: the CD controller's data FIFO.
const DMA_CD: usize = 3;
/// DMA channels 0 and 1: the MDEC's input and output.
const DMA_MDEC_IN: usize = 0;
const DMA_MDEC_OUT: usize = 1;
/// Decoded CD audio kept for the SPU's CD input: a second.
const CD_AUDIO_MAX: usize = 44_100;
const GP0: u32 = 0x1F80_1810;
const GP1: u32 = 0x1F80_1814;
/// Linked-list nodes one GPU DMA may walk before it is called a loop.
const MAX_NODES: u32 = 1 << 20;
/// Memory control (expansion bases and delays, RAM size): kept, no effect.
const MEMCTRL: [std::ops::Range<u32>; 2] = [0x1F80_1000..0x1F80_1024, 0x1F80_1060..0x1F80_1064];

/// The SPU and its clock.
#[derive(Clone, Default)]
pub struct SpuBus {
    pub spu: psx_spu::Spu,
    /// The cycle the SPU has run up to.
    pub at: u64,
    /// Its IRQ line at the last look (for the rising edge).
    pub line: bool,
    /// Mixed output not yet handed to the host.
    pub out: Vec<[i16; 2]>,
    /// The SPU window being recorded, if any.
    pub window: Option<SpuWindow>,
}

/// An SPU window for the recording hook (M6): the register writes that
/// recreate the SPU's state at its start (voices sounding then are keyed
/// on), the game's register writes during it, and SPU RAM at its start.
#[derive(Debug, Clone, Default)]
pub struct SpuWindow {
    /// The SPU cycle it started at.
    pub start: u64,
    pub samples: u32,
    pub regs: Vec<(u16, u16)>,
    /// (sample, register offset, value).
    pub writes: Vec<(u32, u16, u16)>,
    pub ram: Vec<u8>,
}

/// SPU registers a window's start state writes: every voice register,
/// volumes, PMON/NON/EON, the reverb base, the IRQ address, transfer
/// control, CD and external volumes, the reverb set; then SPUCNT (with no
/// transfer mode).
fn spu_state_regs(spu: &psx_spu::Spu) -> Vec<(u16, u16)> {
    let offs = (0..0x180)
        .step_by(2)
        .chain((0x180..0x188).step_by(2))
        .chain((0x190..0x19C).step_by(2))
        .chain([0x1A2, 0x1A4, 0x1AC])
        .chain((0x1B0..0x1B8).step_by(2))
        .chain((0x1C0..0x200).step_by(2));
    let mut out: Vec<(u16, u16)> = offs.map(|o| (o as u16, spu.read(o))).collect();
    out.push((0x1AA, spu.read(0x1AA) & !0x30));
    let sounding = (0..24u32).filter(|v| spu.read(16 * v + 0xC) != 0).fold(0u32, |m, v| m | 1 << v);
    if sounding != 0 {
        out.push((0x188, sounding as u16));
        out.push((0x18A, (sounding >> 16) as u16));
    }
    out
}

impl std::fmt::Debug for SpuBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpuBus").field("at", &self.at).field("line", &self.line).field("out", &self.out.len()).finish()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Io {
    pub irq: Irq,
    pub sio0: Sio0,
    pub dma: Dma,
    pub gpu: Gpu,
    pub spu: SpuBus,
    /// The CD controller (D58), for SDK code that drives the drive itself.
    pub cdc: Cdc,
    /// Its interrupt line at the last look (for the rising edge).
    pub cdc_line: bool,
    /// The MDEC, for libpress (FMV).
    pub mdec: Mdec,
    /// GPU words received (GP0 port and DMA), for the run report.
    pub gp0_words: u64,
    /// Memory-control words written, by address.
    pub memctrl: BTreeMap<u32, u32>,
    /// The last [`RECENT`] accesses: (cycle, address, size, value, write).
    pub recent: std::collections::VecDeque<(u64, u32, u32, u32, bool)>,
    /// Register accesses so far, per chip (D59: what a run emulated).
    pub accesses: BTreeMap<&'static str, u64>,
}

pub const RECENT: usize = 256;

impl Io {
    fn note(&mut self, now: u64, addr: u32, size: u32, v: u32, write: bool) {
        *self.accesses.entry(chip(addr)).or_default() += 1;
        if self.recent.len() == RECENT {
            self.recent.pop_front();
        }
        self.recent.push_back((now, addr, size, v, write));
    }
}

impl<S: Sdk> Machine<S> {
    /// A guest read of `size` bytes of the register at `addr` (physical),
    /// counted per chip for the run report.
    pub fn io_read(&mut self, addr: u32, size: u32) -> Result<u32, String> {
        let v = self.reg_read(addr, size)?;
        let now = self.clock.now;
        if let Some(io) = self.io.as_mut() {
            io.note(now, addr, size, v, false);
        }
        Ok(v)
    }

    /// Read `size` bytes of the register at `addr` (physical) with the
    /// device's own semantics: for guest code ([`Self::io_read`]), and for
    /// adapters that share a device's state with the emulated chips (the
    /// interrupt and DMA controllers, D59), whose accesses aren't guest
    /// I/O.
    pub fn reg_read(&mut self, addr: u32, size: u32) -> Result<u32, String> {
        let now = self.clock.now;
        let video = self.clock.video;
        if SPU.contains(&addr) {
            self.spu_run();
        }
        let Some(io) = self.io.as_mut() else { return Err(no_bus(addr)) };
        io.sio0.update(now, &mut io.irq);
        let v = if SPU.contains(&addr) {
            let off = addr - SPU.start;
            let half = |o: u32| io.spu.spu.read(o) as u32;
            match size {
                4 => half(off & !3) | half((off & !3) + 2) << 16,
                _ => half(off & !1) >> (8 * (addr & 1)),
            }
        } else if Irq::handles(addr) {
            io.irq.read(addr)
        } else if Cdc::handles(addr) {
            // Wider reads (the data FIFO's halfwords) take bytes in turn.
            (0..size.min(4)).fold(0, |v, i| v | (io.cdc.read(addr) as u32) << (8 * i))
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
        } else if addr & !3 == psx_devices::mdec::DATA {
            io.mdec.read()
        } else if addr & !3 == psx_devices::mdec::STATUS {
            io.mdec.status() >> (8 * (addr & 3))
        } else if addr & !3 == GP0 {
            io.gpu.read() >> (8 * (addr & 3))
        } else if addr & !3 == GP1 {
            io.gpu.status() >> (8 * (addr & 3))
        } else {
            return Err(format!("read of unmodelled I/O register {addr:#010x} ({size} bytes)"));
        };
        let v = v & mask(size);
        if Cdc::handles(addr) {
            self.cdc_sync();
        }
        Ok(v)
    }

    /// A guest write of `size` bytes of `v` to the register at `addr`
    /// (physical), counted per chip for the run report.
    pub fn io_write(&mut self, addr: u32, size: u32, v: u32) -> Result<(), String> {
        let now = self.clock.now;
        if let Some(io) = self.io.as_mut() {
            io.note(now, addr, size, v & mask(size), true);
        }
        self.reg_write(addr, size, v)
    }

    /// Write `size` bytes of `v` to the register at `addr` (physical) with
    /// the device's own semantics (see [`Self::reg_read`]).
    pub fn reg_write(&mut self, addr: u32, size: u32, v: u32) -> Result<(), String> {
        let now = self.clock.now;
        let video = self.clock.video;
        if SPU.contains(&addr) {
            self.spu_run();
        }
        let Some(io) = self.io.as_mut() else { return Err(no_bus(addr)) };
        io.sio0.update(now, &mut io.irq);
        let v = v & mask(size);
        if SPU.contains(&addr) {
            let off = addr - SPU.start;
            let halves = match size {
                4 => vec![(off & !3, v as u16), ((off & !3) + 2, (v >> 16) as u16)],
                2 => vec![(off & !1, v as u16)],
                _ => {
                    // A byte store: the halfword with that byte replaced.
                    let o = off & !1;
                    let old = io.spu.spu.read(o);
                    let sh = 8 * (addr & 1);
                    vec![(o, (old & !(0xFF << sh)) | ((v as u16 & 0xFF) << sh))]
                }
            };
            for (o, h) in halves {
                if let Some(w) = io.spu.window.as_mut() {
                    if o == 0x1A8 {
                        // SPU RAM changes through the data port.
                        io.spu.window = None;
                    } else {
                        let sample = ((io.spu.at - w.start) / psx_spu::CYCLES_PER_SAMPLE as u64) as u32;
                        w.writes.push((sample, o as u16, h));
                    }
                }
                io.spu.spu.write(o, h);
            }
            self.spu_irq();
        } else if Irq::handles(addr) {
            io.irq.write(addr, size, v);
        } else if Cdc::handles(addr) {
            io.cdc.write(addr, v as u8, now);
            self.cdc_sync();
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
        } else if Mdec::handles(addr) {
            if addr & !3 == psx_devices::mdec::DATA {
                io.mdec.write(v);
            } else {
                io.mdec.control(v);
            }
            // Output a waiting DMA1 can take now.
            self.run_dma()?;
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

    /// Run every DMA channel that is ready, and whose device can serve it:
    /// MDEC output waits (as its request line does) while a decode is
    /// still taking input and hasn't produced the words asked for.
    fn run_dma(&mut self) -> Result<(), String> {
        let waits = |io: &Io, ch: usize| ch == DMA_MDEC_OUT && io.mdec.busy() && io.mdec.available() < io.dma.words(ch) as usize;
        while let Some(ch) = self.io.as_ref().and_then(|io| (0..7).find(|&c| io.dma.ready(c) && !waits(io, c))) {
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
        if ch == DMA_SPU {
            self.spu_run();
        }
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
                let mut recording = self.gpu_record_start();
                let mut nodes = 0;
                loop {
                    let header = self.mem.r32(addr);
                    let word = |m: &Self, i: u32| m.mem.r32(addr.wrapping_add(4 * i) & 0x1F_FFFC);
                    for i in 1..=(header >> 24) {
                        let w = word(self, i);
                        self.gp0(w);
                    }
                    if let Some((_, rec)) = recording.as_mut() {
                        rec.push((1..=(header >> 24)).map(|i| word(self, i)).collect());
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
                if let Some((state, rec)) = recording {
                    self.host.gpu_record(&state, &rec);
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
            (DMA_MDEC_IN, 0 | 1, true) => {
                for _ in 0..d.words(ch) {
                    let w = self.mem.r32(addr);
                    if let Some(io) = self.io.as_mut() {
                        io.mdec.write(w);
                    }
                    addr = addr.wrapping_add(step) & 0x1F_FFFC;
                }
            }
            (DMA_MDEC_OUT, 0 | 1, false) => {
                for _ in 0..d.words(ch) {
                    let w = self.io.as_mut().map_or(0, |io| io.mdec.read());
                    self.mem.w32(addr, w);
                    addr = addr.wrapping_add(step) & 0x1F_FFFC;
                }
            }
            (DMA_CD, 0 | 1, false) => {
                for _ in 0..d.words(ch) {
                    let w = self.io.as_mut().map_or(0, |io| io.cdc.fifo_word());
                    self.mem.w32(addr, w);
                    addr = addr.wrapping_add(4) & 0x1F_FFFC;
                }
            }
            (DMA_SPU, 0 | 1, true) => {
                let words: Vec<u32> = (0..d.words(ch)).map(|i| self.mem.r32(addr.wrapping_add(4 * i) & 0x1F_FFFC)).collect();
                if let Some(io) = self.io.as_mut() {
                    io.spu.spu.dma_write(&words);
                    io.spu.window = None;
                }
                addr = addr.wrapping_add(4 * words.len() as u32) & 0x1F_FFFC;
                self.spu_irq();
            }
            (DMA_SPU, 0 | 1, false) => {
                let mut words = vec![0u32; d.words(ch) as usize];
                if let Some(io) = self.io.as_mut() {
                    io.spu.spu.dma_read(&mut words);
                }
                for w in words {
                    self.mem.w32(addr, w);
                    addr = addr.wrapping_add(4) & 0x1F_FFFC;
                }
                self.spu_irq();
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

    /// When the run records GPU batches and the host wants this one: the
    /// state it starts from, with everything before it submitted.
    fn gpu_record_start(&mut self) -> Option<(Vec<u32>, Vec<Vec<u32>>)> {
        if self.opts.trace & psxw_abi::BootOptions::TRACE_RECORD_GPU == 0 {
            return None;
        }
        let io = self.io.as_mut()?;
        let state = io.gpu.recordable_state()?;
        let words = io.gpu.take_commands();
        if !words.is_empty() {
            self.host.render_submit(&words);
        }
        self.host.gpu_record_begin().then(|| (state.to_vec(), Vec::new()))
    }

    /// Run the SPU up to the current cycle: one tick per 768 cycles, each
    /// with the CD's next output frame as its CD input.
    pub fn spu_run(&mut self) {
        let now = self.clock.now;
        let Some(io) = self.io.as_mut() else { return };
        const CYCLES: u64 = psx_spu::CYCLES_PER_SAMPLE as u64;
        let n = now.saturating_sub(io.spu.at) / CYCLES;
        for _ in 0..n {
            let [l, r] = self.cd_audio.pop_front().unwrap_or([0, 0]);
            io.spu.spu.set_cd_input(l, r);
            let f = io.spu.spu.tick();
            io.spu.out.push(f);
        }
        io.spu.at += n * CYCLES;
        let done = io.spu.window.as_ref().is_some_and(|w| io.spu.at - w.start >= w.samples as u64 * CYCLES);
        if done {
            if let Some(w) = io.spu.window.take() {
                self.host.spu_record(&w);
            }
        }
        self.spu_irq();
    }

    /// At a vblank, while the run records SPU windows and none is open:
    /// open one if the host wants it.
    pub fn spu_record_vblank(&mut self) {
        if self.opts.trace & psxw_abi::BootOptions::TRACE_RECORD_SPU == 0 || self.io.as_ref().is_none_or(|io| io.spu.window.is_some()) {
            return;
        }
        let samples = self.host.spu_record_begin();
        let Some(io) = self.io.as_mut() else { return };
        if samples > 0 {
            let ram = io.spu.spu.ram().iter().flat_map(|h| h.to_le_bytes()).collect();
            io.spu.window = Some(SpuWindow { start: io.spu.at, samples, regs: spu_state_regs(&io.spu.spu), writes: Vec::new(), ram });
        }
    }

    /// Bring the CD controller up to now: its due work (reading sectors
    /// through the host), its decoded audio to the SPU's CD input, its next
    /// wakeup on the clock, and IRQ2 when its line rises.
    pub fn cdc_sync(&mut self) {
        let now = self.clock.now;
        let timing = self.drive.timing;
        let Machine { io, host, cd_audio, clock, .. } = self;
        let Some(io) = io.as_mut() else { return };
        io.cdc.drive.timing = timing;
        io.cdc.run(now, &mut crate::bios::HostDisc(&mut **host));
        cd_audio.extend(io.cdc.audio.drain(..));
        let over = cd_audio.len().saturating_sub(CD_AUDIO_MAX);
        cd_audio.drain(..over);
        clock.cancel(|e| *e == crate::machine::Event::Cdc);
        if let Some(t) = io.cdc.next_due() {
            clock.schedule(t.max(now), crate::machine::Event::Cdc);
        }
        let line = io.cdc.line();
        let rising = line && !io.cdc_line;
        io.cdc_line = line;
        if rising {
            self.raise_irq(irq::CDROM);
        }
    }

    /// Tell the CD controller about the disc: its tracks, and the region
    /// its license sector names (GetID).
    pub fn cdc_insert(&mut self) {
        let info = self.host.disc_info();
        let mut lic = [0u8; psx_devices::cd::RAW];
        let region = if self.host.disc_read(4, &mut lic) {
            let text = String::from_utf8_lossy(psx_devices::cd::user_data(&lic)).to_ascii_lowercase();
            if text.contains("amer") {
                *b"SCEA"
            } else if text.contains("euro") {
                *b"SCEE"
            } else {
                *b"SCEI"
            }
        } else {
            *b"SCEI"
        };
        let Some(io) = self.io.as_mut() else { return };
        io.cdc.region = region;
        if let Some(i) = info.filter(|i| i.tracks > 0) {
            io.cdc.tracks = i.track_lba[..i.tracks.min(99) as usize].to_vec();
            io.cdc.end = i.sectors;
        }
    }

    /// The SPU's IRQ line rising raises IRQ9.
    fn spu_irq(&mut self) {
        let Some(io) = self.io.as_mut() else { return };
        let line = io.spu.spu.irq();
        let rising = line && !io.spu.line;
        io.spu.line = line;
        if rising {
            self.raise_irq(irq::SPU);
        }
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
    /// then the SDK's dispatcher when adapters run it, then its exit.
    pub fn queue_irq_chains(&mut self) {
        for prio in 0..4u32 {
            let Some(chain) = self.kernel.int_handlers.get(&prio) else { continue };
            for &h in chain.iter().rev() {
                // An element the adapters own runs as their step, in its
                // place in the chain.
                if let Some(s) = S::chain_element(self, h) {
                    self.irq_calls.push_back(IrqStep::Sdk(s));
                    continue;
                }
                let verifier = self.mem.r32(h + 8);
                let handler = self.mem.r32(h + 4);
                if verifier != 0 {
                    self.irq_calls.push_back(IrqStep::Chain(GuestCall { addr: verifier, args: [0; 4] }, (handler != 0).then_some(handler)));
                }
            }
        }
        if let Some(s) = S::irq_dispatch(self) {
            self.irq_calls.push_back(IrqStep::Sdk(s));
        }
        self.irq_calls.push_back(IrqStep::Exit);
    }
}

/// The chip a register belongs to, for the run report.
pub fn chip(addr: u32) -> &'static str {
    match addr {
        0x1F80_1040..0x1F80_1050 => "SIO0",
        0x1F80_1050..0x1F80_1060 => "SIO1",
        0x1F80_1070..0x1F80_1078 => "IRQ",
        0x1F80_1080..0x1F80_1100 => "DMA",
        0x1F80_1100..0x1F80_1130 => "timers",
        0x1F80_1800..0x1F80_1804 => "CD",
        0x1F80_1810..0x1F80_1818 => "GPU",
        0x1F80_1820..0x1F80_1828 => "MDEC",
        0x1F80_1C00..0x1F80_2000 => "SPU",
        _ => "other",
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
        // The SPU: main volume left reads back; the CD controller's status
        // (index 0, parameter FIFO empty and not full).
        m.io_write(0x1F80_1D80, 2, 0x3FFF).unwrap();
        assert_eq!(m.io_read(0x1F80_1D80, 2).unwrap(), 0x3FFF);
        assert_eq!(m.io_read(0x1F80_1800, 1).unwrap(), 0x18);
        // GP1(08h) display mode, read back in GPUSTAT bits 17-22.
        m.io_write(0x1F80_1814, 4, 0x0800_0001).unwrap();
        assert_eq!((m.io_read(0x1F80_1814, 4).unwrap() >> 17) & 3, 1);
        m.io_write(0x1F80_1124, 2, 0x0200).unwrap();
        assert_eq!(m.io_read(0x1F80_1124, 2).unwrap(), 0x0600);
    }
}
