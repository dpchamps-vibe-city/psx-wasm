//! The machine the BIOS and the SDK adapters act on: guest memory, the
//! host, devices on the virtual timeline, kernel state, and the queue of
//! guest calls (callbacks) to run.
//!
//! Nothing here calls into the guest. Callbacks are queued, and a call that
//! must wait returns [`Flow::Block`]: the runtime's driver advances time,
//! runs queued callbacks with no borrow held, and resumes. That is what
//! makes re-entry from callbacks safe (plans/M02 "Callback re-entrancy").

use crate::kernel::Kernel;
use psx_devices::cd::Drive;
use psx_devices::clock::{Clock, Video};
use psx_devices::mem::Mem;
use psx_devices::timers::Timer;
use psxw_abi::BootOptions;
use std::collections::{BTreeMap, VecDeque};

/// What the runtime needs from its host.
pub trait Host {
    fn log(&mut self, level: u32, msg: &str);
    /// Pacing at vblank `frame`; a host may stop the run here.
    fn wait_vblank(&mut self, frame: u64);
    /// Raw 2352-byte sectors; false when unreadable.
    fn disc_read(&mut self, lba: u32, out: &mut [u8; 2352]) -> bool;
    fn has_disc(&mut self) -> bool;
    /// The pad state for `port` (0 or 1): bytes as the controller sends
    /// them after its ID (e.g. `[0xFF, 0xFF]` for a digital pad, nothing
    /// pressed). Empty when nothing is connected.
    fn pad_state(&mut self, port: u32) -> Vec<u8>;
    fn storage_read(&mut self, name: &str) -> Option<Vec<u8>>;
    fn storage_write(&mut self, name: &str, data: &[u8]) -> bool;
    /// A structured run-report event (JSON).
    fn report(&mut self, json: &str);
    /// Renderer commands (`psxw_abi::render`) to execute.
    fn render_submit(&mut self, _words: &[u32]) {}
    /// A VRAM rectangle after everything submitted, `w * h` pixels.
    fn render_readback(&mut self, _x: u32, _y: u32, w: u32, h: u32) -> Vec<u16> {
        vec![0; (w * h) as usize]
    }
    /// Show a frame.
    fn present(&mut self, _desc: &psxw_abi::render::PresentDesc) {}
    /// Audio frames (left, right) at `psxw_abi::AUDIO_HZ`.
    fn audio(&mut self, _frames: &[[i16; 2]]) {}
}

/// The guest's registers the runtime shares.
pub trait Regs {
    fn sp(&self) -> u32;
    fn set_sp(&mut self, v: u32);
    fn gp(&self) -> u32;
    fn set_gp(&mut self, v: u32);
}

/// A guest function to call with o32 arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestCall {
    pub addr: u32,
    pub args: [u32; 4],
}

/// One step of the BIOS exception handler (psx-spx "BIOS Interrupt/
/// Exception Handling"): every `SysEnqIntRP` element, priority 0 to 3,
/// then the exit. A ReturnFromException in an element skips the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqStep {
    /// An element's first function, and its second to call with the
    /// first's result when that is nonzero.
    Chain(GuestCall, Option<u32>),
    /// The end of the handler: the custom exit (`HookEntryInt`, B0h:19h)
    /// if one is set, entered like a longjmp to its buffer with 1.
    Exit,
}

/// Events on the virtual timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Vertical blank (IRQ0).
    Vblank,
    /// Root counter `n` reached its IRQ condition (IRQ4-6).
    Timer(u8),
    /// A device event the SDK adapters own: (kind, data).
    Sdk(u32, u32),
}

/// What a BIOS call or adapter does next.
pub enum Flow<S> {
    Return(u64),
    /// Wait until `until` holds (time advancing, callbacks running), then
    /// continue with `then`.
    Block { until: Wait, then: Box<dyn FnOnce(&mut Machine<S>) -> Flow<S>> },
}

impl<S> Flow<S> {
    pub fn ret(v: u32) -> Self {
        Flow::Return(v as u64)
    }
}

/// Conditions a blocked call waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// `vblanks >= n`.
    Vblanks(u64),
    /// Virtual time reaches the cycle.
    Cycle(u64),
    /// An SDK-defined condition, polled through [`Sdk::ready`].
    Sdk(u32),
    /// A BIOS event becomes ready.
    Event(u32),
}

/// SDK adapter state plugs into the machine through this trait.
pub trait Sdk: Sized {
    /// Handle an `Event::Sdk` the adapters scheduled.
    fn on_event(m: &mut Machine<Self>, kind: u32, data: u32);
    /// Whether `Wait::Sdk(cond)` holds.
    fn ready(m: &Machine<Self>, cond: u32) -> bool;
    /// Called at every vblank, before callbacks are delivered.
    fn on_vblank(m: &mut Machine<Self>);
    /// A DMA channel finished a transfer (device libraries' DMA).
    fn on_dma(_m: &mut Machine<Self>, _ch: usize) {}
}

pub struct Machine<S> {
    pub mem: Box<dyn Mem>,
    pub host: Box<dyn Host>,
    pub regs: Box<dyn Regs>,
    pub opts: BootOptions,
    pub clock: Clock<Event>,
    pub timers: [Timer; 3],
    pub drive: Drive,
    /// The CD's audio output at 44.1 kHz (left, right): decoded XA, for
    /// the SPU's CD input to take. Bounded: the oldest samples drop.
    pub cd_audio: VecDeque<[i16; 2]>,
    /// `Mute`/`Demute`: the drive's audio output is silenced.
    pub cd_mute: bool,
    /// The cycle up to which audio has been produced.
    pub audio_at: u64,
    /// Vblanks so far.
    pub vblanks: u64,
    pub kernel: Kernel,
    /// Callbacks to run, in order.
    pub calls: VecDeque<GuestCall>,
    /// The exception handler's steps, run before anything else queued
    /// ([`crate::io`]).
    pub irq_calls: VecDeque<IrqStep>,
    /// ReturnFromException ran in the guest call now unwinding.
    pub rfe: bool,
    /// Register-level devices for original SDK code; `None` traps on I/O.
    pub io: Option<crate::io::Io>,
    /// Interrupts are disabled (critical section): events wait.
    pub crit: bool,
    /// A callback (interrupt handler) is running: no nested delivery.
    pub in_irq: bool,
    pub sdk: S,
    /// Calls per BIOS function and adapter, for the run report.
    pub counts: BTreeMap<String, u64>,
}

impl<S: Sdk> Machine<S> {
    pub fn new(mem: Box<dyn Mem>, host: Box<dyn Host>, regs: Box<dyn Regs>, opts: BootOptions, sdk: S) -> Self {
        let video = if opts.video == 1 { Video::Pal } else { Video::Ntsc };
        let mut drive = Drive::default();
        if opts.cd_timing == 1 {
            drive.timing = psx_devices::cd::Timing::Instant;
        }
        let mut m = Self {
            mem,
            host,
            regs,
            opts,
            clock: Clock::new(video),
            timers: [Timer::default(); 3],
            drive,
            cd_audio: VecDeque::new(),
            cd_mute: false,
            audio_at: 0,
            vblanks: 0,
            kernel: Kernel::default(),
            calls: VecDeque::new(),
            irq_calls: VecDeque::new(),
            rfe: false,
            io: None,
            crit: false,
            in_irq: false,
            sdk,
            counts: BTreeMap::new(),
        };
        let first = m.clock.video.vblank_at(1);
        m.clock.schedule(first, Event::Vblank);
        m
    }

    pub fn count(&mut self, what: &str) {
        *self.counts.entry(what.to_string()).or_default() += 1;
    }

    pub fn log(&mut self, msg: &str) {
        self.host.log(1, msg);
    }

    /// Argument word `k` of an o32 call (k >= 4 from the guest stack).
    pub fn arg(&self, a: &[u32; 4], k: usize) -> u32 {
        if k < 4 { a[k] } else { self.mem.r32(self.regs.sp().wrapping_add(4 * k as u32)) }
    }

    /// Whether a wait condition holds.
    pub fn holds(&self, w: Wait) -> bool {
        match w {
            Wait::Vblanks(n) => self.vblanks >= n,
            Wait::Cycle(t) => self.clock.now >= t,
            Wait::Sdk(c) => S::ready(self, c),
            Wait::Event(h) => self.kernel.event_ready(h),
        }
    }

    /// Deliver every event due by `until` (unless interrupts are masked
    /// or a handler is running), then move time to `until`. Returns
    /// whether any event was delivered.
    pub fn run_until(&mut self, until: u64) -> bool {
        let mut any = false;
        if !self.crit && !self.in_irq {
            while let Some(e) = self.clock.pop_until(until) {
                self.deliver(e);
                any = true;
            }
        }
        self.clock.advance_to(until);
        any
    }

    /// Advance to the next event (or `limit`) and deliver it.
    pub fn step(&mut self, limit: u64) {
        let next = self.clock.next_time().unwrap_or(limit).min(limit);
        if self.crit || self.in_irq {
            // Time passes; interrupts wait for the critical section to end.
            self.clock.advance_to(next.max(self.clock.now + 1));
            return;
        }
        self.run_until(next);
    }

    fn deliver(&mut self, e: Event) {
        match e {
            Event::Vblank => {
                self.vblanks += 1;
                let next = self.clock.video.vblank_at(self.vblanks + 1);
                self.clock.schedule(next, Event::Vblank);
                S::on_vblank(self);
                self.kernel.pad_vblank(&mut *self.mem, &mut *self.host);
                // The kernel's vblank handler delivers RCntCNT3 events.
                self.deliver_event(0xF200_0003, 0x0002);
                self.raise_irq(psx_devices::irq::VBLANK);
                self.gpu_vblank();
                self.card_vblank();
                self.audio_vblank();
                self.host.wait_vblank(self.vblanks);
            }
            Event::Timer(n) => {
                let now = self.clock.now;
                let video = self.clock.video;
                self.timers[n as usize].fired = true;
                if let Some(t) = self.timers[n as usize].next_irq(n as usize, video, now + 1) {
                    self.clock.schedule(t, Event::Timer(n));
                }
                if self.kernel.timer_irq_enabled(n) {
                    self.deliver_event(0xF200_0000 | n as u32, 0x0002);
                }
                self.raise_irq(psx_devices::irq::TIMER0 + n as u32);
            }
            Event::Sdk(kind, data) => S::on_event(self, kind, data),
        }
    }

    /// Hand the host the audio that fell due since the last vblank: one
    /// frame per 768 cycles of virtual time, the SPU's mix (with the CD's
    /// output as its CD input). Without a bus, the CD's output alone.
    fn audio_vblank(&mut self) {
        if self.io.is_some() {
            self.spu_run();
            let frames = self.io.as_mut().map(|io| std::mem::take(&mut io.spu.out)).unwrap_or_default();
            self.host.audio(&frames);
            return;
        }
        const CYCLES: u64 = psx_devices::clock::CPU_HZ / psxw_abi::AUDIO_HZ as u64;
        let n = (self.clock.now - self.audio_at) / CYCLES;
        self.audio_at += n * CYCLES;
        let frames: Vec<[i16; 2]> = (0..n).map(|_| self.cd_audio.pop_front().unwrap_or([0, 0])).collect();
        self.host.audio(&frames);
    }

    /// (Re)schedule root counter `n`'s next IRQ.
    pub fn reschedule_timer(&mut self, n: usize) {
        self.clock.cancel(|e| *e == Event::Timer(n as u8));
        let video = self.clock.video;
        if let Some(t) = self.timers[n].next_irq(n, video, self.clock.now) {
            self.clock.schedule(t, Event::Timer(n as u8));
        }
    }

    /// BIOS DeliverEvent: callbacks for interrupt-mode events, ready status
    /// for the rest.
    pub fn deliver_event(&mut self, class: u32, spec: u32) {
        let calls = self.kernel.deliver(class, spec);
        if class & 0xFF00_0000 != 0xF200_0000 {
            self.count(&format!("event.{class:08x}/{spec:x} -> {} callbacks", calls.len()));
        }
        self.calls.extend(calls.into_iter().map(|addr| GuestCall { addr, args: [0; 4] }));
    }
}
