//! The SPU in the oracle (plans/M06-sound.md tasks 1–3).
//!
//! [`Device`] is psx-spu as a [`DeviceUnderTest`] for SPU_RUN vectors. It
//! reproduces the runner's ops (`oracle/runner/spu.c`) on a psx-spu device:
//! SPU_RESET with its writes and waits, SPU_RAM_WRITE by DMA, and SPU_RUN
//! with the runner's timing contract. The parts of that contract that are
//! not sample-aligned (writes that queue up behind each other, the register
//! snapshot taken while the SPU keeps running, the captures DMA-read while
//! it keeps running) follow [`Timing`], a model of the runner's CPU time
//! calibrated against the emulator (plans/M06-sound.md, Findings).

use crate::commands::DeviceUnderTest;
use crate::script::MAX_OP_PAYLOAD;
use crate::store::Store;
use crate::vector::{SpuRun, Vector};
use crate::wire;
use anyhow::{Result, bail};
use psx_spu::{Spu, reg};

/// The runner's silent block (SPU RAM 1000h): flags 07h, data zero.
const SILENT: u32 = 0x1000;

/// The runner's CPU time around an SPU_RUN, in CPU cycles. Time 0 is the
/// timer start right after the runner saw SPUSTAT bit 11 rise; the SPU's
/// sample ticks fall at `768 m - phase` (m = 1, 2, …). An access at time
/// t sees every tick at or before t.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timing {
    /// Ticks lead the timer by this much (the sync loop's latency).
    pub phase: f64,
    /// A write at sample s with nothing queued happens at 768 s + this.
    pub write: f64,
    /// Cycles from one queued write to the next.
    pub write_gap: f64,
    /// A write reaches the SPU's state this long after the CPU makes it.
    pub write_latency: f64,
    /// With no queued write, the snapshot loop starts at 768 N + this.
    pub end: f64,
    /// After a write at the last sample, the snapshot starts this much
    /// after that write.
    pub end_after_write: f64,
    /// Cycles from one snapshot read to the next.
    pub read_gap: f64,
    /// Cycles the DATA register's iteration (no read) saves.
    pub data_skip: f64,
    /// From the last snapshot read until the runner waits for DMA-read
    /// mode (the first capture's first FIFO load is at the next tick).
    pub capture_start: f64,
    /// Ticks between a capture's last FIFO load and the next capture's
    /// first, beyond the next tick.
    pub capture_gap: f64,
    /// Robustness checks: move every FIFO load this many ticks (the
    /// emulator's loads land a tick early or late now and then).
    pub capture_slip: f64,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            phase: 0.0,
            write: 60.0,
            write_gap: 41.0,
            write_latency: 0.0,
            end: 100.0,
            end_after_write: 100.0,
            read_gap: 28.58,
            data_skip: -60.0,
            capture_start: 105.0,
            capture_gap: 2.0,
            capture_slip: 0.0,
        }
    }
}

impl Timing {
    /// Parse `name=value,…` overrides (calibration tools).
    pub fn with(mut self, spec: &str) -> Result<Self> {
        for kv in spec.split(',').filter(|s| !s.is_empty()) {
            let Some((k, v)) = kv.split_once('=') else { bail!("bad timing override {kv}") };
            let v: f64 = v.parse()?;
            match k {
                "phase" => self.phase = v,
                "write" => self.write = v,
                "write_gap" => self.write_gap = v,
                "write_latency" => self.write_latency = v,
                "end" => self.end = v,
                "end_after_write" => self.end_after_write = v,
                "read_gap" => self.read_gap = v,
                "data_skip" => self.data_skip = v,
                "capture_start" => self.capture_start = v,
                "capture_gap" => self.capture_gap = v,
                "capture_slip" => self.capture_slip = v,
                _ => bail!("unknown timing field {k}"),
            }
        }
        Ok(self)
    }
}

/// A psx-spu device driven on the runner's clock.
#[derive(Clone)]
struct Clock {
    spu: Spu,
    /// Run ticks done (m).
    m: u64,
    phase: f64,
    /// Register writes on their way to the SPU: (time they land, register,
    /// value), in order.
    pending: std::collections::VecDeque<(f64, u32, u16)>,
}

impl Clock {
    /// Run every tick, and land every write, at or before `t`.
    fn until(&mut self, t: f64) {
        loop {
            let tick = 768.0 * (self.m + 1) as f64 - self.phase;
            match self.pending.front() {
                Some(&(land, r, v)) if land <= t && land <= tick => {
                    self.spu.write(r, v);
                    self.pending.pop_front();
                }
                _ if tick <= t => {
                    self.spu.tick();
                    self.m += 1;
                }
                _ => break,
            }
        }
    }
}

/// `set_transfer(mode)` in the runner.
fn set_transfer(spu: &mut Spu, mode: u16) {
    let c = spu.read(reg::SPUCNT);
    spu.write(reg::SPUCNT, (c & !0x30) | mode << 4);
}

/// `spu_dma` writing `bytes` at `addr` (no time passes in the model: the
/// SPU state these ticks could change is not observable after a reset).
fn dma_write(spu: &mut Spu, addr: u32, bytes: &[u8]) {
    set_transfer(spu, 0);
    spu.write(reg::RAMCNT, 4);
    spu.write(reg::TSA, (addr >> 3) as u16);
    set_transfer(spu, 2);
    for h in bytes.chunks(2) {
        spu.transfer_write(u16::from_le_bytes([h[0], *h.get(1).unwrap_or(&0)]));
    }
    set_transfer(spu, 0);
}

/// SPU_RESET as the runner performs it (oracle/runner/spu.c).
pub fn reset(spu: &mut Spu) {
    spu.write(reg::SPUCNT, 0x8000);
    spu.write(reg::RAMCNT, 0x0004);
    spu.write(reg::KOFF, 0xFFFF);
    spu.write(reg::KOFF + 2, 0x00FF);
    for _ in 0..2 {
        spu.tick();
    }
    let mut silent = [0u8; 16];
    silent[1] = 0x07;
    dma_write(spu, SILENT, &silent);
    for v in 0..24 {
        spu.write(reg::voice(v, reg::VOLL), 0);
        spu.write(reg::voice(v, reg::VOLR), 0);
        spu.write(reg::voice(v, reg::PITCH), 0x1000);
        spu.write(reg::voice(v, reg::SSA), (SILENT >> 3) as u16);
        spu.write(reg::voice(v, reg::ADSR1), 0);
        spu.write(reg::voice(v, reg::ADSR2), 0);
        spu.write(reg::voice(v, reg::LSAX), (SILENT >> 3) as u16);
    }
    for off in (0x180..0x1A0).step_by(2) {
        spu.write(off, 0);
    }
    spu.write(reg::ESA, 0);
    spu.write(reg::IRQA, 0);
    for off in (0x1B0..0x1B8).step_by(2) {
        spu.write(off, 0);
    }
    for off in (0x1C0..0x200).step_by(2) {
        spu.write(off, 0);
    }
    spu.write(reg::KON, 0xFFFF);
    spu.write(reg::KON + 2, 0x00FF);
    for _ in 0..64 {
        spu.tick();
    }
    spu.write(reg::KOFF, 0xFFFF);
    spu.write(reg::KOFF + 2, 0x00FF);
    for _ in 0..64 {
        spu.tick();
    }
    for v in 0..24 {
        spu.write(reg::voice(v, reg::ENVX), 0);
    }
    spu.write(reg::KON, 0);
    spu.write(reg::KON + 2, 0);
    spu.write(reg::KOFF, 0);
    spu.write(reg::KOFF + 2, 0);
}

/// The device after SPU_RESET and the vector's SPU RAM writes, at the
/// capture-buffer half edge SPU_RUN waits for (time 0 of the run).
pub fn at_edge(r: &SpuRun, store: &Store) -> Result<Spu> {
    let mut spu = Spu::new();
    reset(&mut spu);
    let chunk = (MAX_OP_PAYLOAD - 8) & !(wire::SPU_LEN_ALIGN as usize - 1);
    for ram in &r.ram {
        let bytes = store.blob(&ram.bytes)?;
        for (i, part) in bytes.chunks(chunk).enumerate() {
            dma_write(&mut spu, ram.addr + (i * chunk) as u32, part);
        }
    }
    // Start at a capture-buffer half edge: SPUSTAT bit 11 rising.
    let mut prev = spu.status() & 0x800;
    loop {
        spu.tick();
        let cur = spu.status() & 0x800;
        if cur != 0 && prev == 0 {
            break;
        }
        prev = cur;
    }
    Ok(spu)
}

/// A run in progress: the device on the runner's clock with the timed
/// writes queued, before the snapshot.
#[derive(Clone)]
pub struct Written {
    clock: Clock,
    /// When the CPU made the last write (None: no writes).
    last: Option<f64>,
}

/// Queue the timed writes: the CPU makes them at their sample's time, or
/// right after the write before when they queue up.
pub fn written(spu: Spu, r: &SpuRun, t: &Timing) -> Written {
    let mut c = Clock { spu, m: 0, phase: t.phase, pending: Default::default() };
    let mut last: Option<f64> = None;
    for w in &r.writes {
        let at = 768.0 * w.sample as f64 + t.write;
        let when = match last {
            Some(p) if p + t.write_gap > at => p + t.write_gap,
            _ => at,
        };
        c.pending.push_back((when + t.write_latency, w.reg, w.value));
        last = Some(when);
    }
    Written { clock: c, last }
}

/// The snapshot and the captures, read while the SPU keeps running.
pub fn readback(w: Written, r: &SpuRun, t: &Timing) -> Vec<u8> {
    let Written { clock: mut c, last } = w;
    c.phase = t.phase;
    let end = 768.0 * r.samples as f64 + t.end;
    let start = match last {
        Some(p) if p + t.end_after_write > end => p + t.end_after_write,
        _ => end,
    };
    let mut out = vec![0u8; wire::SPU_SNAPSHOT_SIZE as usize];
    let mut now = start;
    for j in 0..(wire::SPU_SNAPSHOT_SIZE / 2) {
        let off = 2 * j;
        let v = if off == reg::DATA {
            now -= t.data_skip;
            0
        } else {
            c.until(now);
            c.spu.read(off)
        };
        out[off as usize..off as usize + 2].copy_from_slice(&v.to_le_bytes());
        now += t.read_gap;
    }
    // The captures. The runner's set_transfer(3) waits for SPUSTAT's mode
    // bits, which the SPU updates at its next tick; from then on the DMA
    // receives one FIFO load of 32 halfwords per tick, read after that
    // tick. After a capture, the busy flag and set_transfer(0) take
    // `capture_gap` more ticks before the next capture's first load.
    let mut m = 1 + ((now - t.read_gap + t.capture_start + t.phase) / 768.0).floor() as u64;
    for &(addr, len) in &r.captures {
        if len == 0 {
            continue;
        }
        set_transfer(&mut c.spu, 0);
        c.spu.write(reg::RAMCNT, 4);
        c.spu.write(reg::TSA, (addr >> 3) as u16);
        set_transfer(&mut c.spu, 3);
        let halfwords = len / 2;
        let loads = halfwords.div_ceil(FIFO);
        for g in 0..loads {
            c.until(768.0 * ((m + g as u64) as f64 + t.capture_slip) - c.phase);
            for _ in 0..FIFO.min(halfwords - g * FIFO) {
                out.extend_from_slice(&c.spu.transfer_read().to_le_bytes());
            }
        }
        set_transfer(&mut c.spu, 0);
        m += loads as u64 + t.capture_gap as u64;
    }
    out
}

/// Halfwords the emulator's SPU hands the DMA per tick.
const FIFO: u32 = 32;

/// Run an SPU_RUN vector the way the runner does, on a powered-on device.
pub fn run(r: &SpuRun, store: &Store, t: &Timing) -> Result<Vec<u8>> {
    let spu = at_edge(r, store)?;
    Ok(readback(written(spu, r, t), r, t))
}

/// psx-spu as the oracle's `spu` device.
#[derive(Default)]
pub struct Device {
    pub timing: Timing,
}

impl DeviceUnderTest for Device {
    fn device(&self) -> &str {
        "spu"
    }

    fn name(&self) -> String {
        "psx-spu".into()
    }

    fn run(&mut self, v: &Vector, store: &Store) -> Result<Vec<u8>> {
        match v {
            Vector::SpuRun(r) => run(r, store, &self.timing),
            other => bail!("the spu device cannot run a {} vector", other.kind()),
        }
    }
}

impl Written {
    /// Calibration: run (and land queued writes) up to and including run
    /// tick `m`.
    pub fn step_to(&mut self, m: u64) {
        let t = 768.0 * m as f64 - self.clock.phase;
        self.clock.until(t);
    }

    pub fn spu(&self) -> &Spu {
        &self.clock.spu
    }

    pub fn last_write(&self) -> Option<f64> {
        self.last
    }
}
