//! Kernel state behind the BIOS functions: event control blocks, the pad
//! buffers the kernel fills at vblank, open files, the heap, interrupt
//! handler queues (psx-spx "Kernel (BIOS)").

use crate::machine::Host;
use psx_devices::mem::Mem;
use std::collections::BTreeMap;

pub const EV_UNUSED: u32 = 0x0000;
pub const EV_DISABLED: u32 = 0x1000;
pub const EV_ENABLED: u32 = 0x2000;
pub const EV_READY: u32 = 0x4000;
/// Event mode: call the handler from the interrupt.
pub const EV_MODE_INTR: u32 = 0x1000;
pub const EV_HANDLE: u32 = 0xF100_0000;
/// Default number of event control blocks (SYSTEM.CNF `EVENT=`).
pub const EVENTS: usize = 16;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EvCb {
    pub class: u32,
    pub spec: u32,
    pub mode: u32,
    pub func: u32,
    pub status: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct File {
    pub lba: u32,
    pub size: u32,
    pub pos: u32,
}

#[derive(Debug, Clone, Default)]
pub struct Kernel {
    pub events: Vec<EvCb>,
    /// `InitPAD` buffers: (address, size) per port.
    pub pad_bufs: Option<[(u32, u32); 2]>,
    pub pad_started: bool,
    pub heap: (u32, u32),
    pub files: BTreeMap<u32, File>,
    /// Root counter IRQs unmasked (StartRCnt).
    pub timer_irq: [bool; 3],
    /// `ChangeClearRCnt` / `ChangeClearPad` flags.
    pub clear_rcnt: [u32; 4],
    pub clear_pad: u32,
    pub custom_exit: u32,
    /// `SysEnqIntRP` queues: priority → handler structs.
    pub int_handlers: BTreeMap<u32, Vec<u32>>,
    /// Memory cards and their files.
    pub cards: crate::card::Cards,
}

impl Kernel {
    fn ensure(&mut self) {
        if self.events.is_empty() {
            self.events = vec![EvCb::default(); EVENTS];
        }
    }

    fn index(h: u32) -> Option<usize> {
        (h & 0xFFFF_0000 == EV_HANDLE).then_some((h & 0xFFFF) as usize)
    }

    pub fn open_event(&mut self, class: u32, spec: u32, mode: u32, func: u32) -> u32 {
        self.ensure();
        match self.events.iter().position(|e| e.status == EV_UNUSED) {
            Some(i) => {
                self.events[i] = EvCb { class, spec, mode, func, status: EV_DISABLED };
                EV_HANDLE | i as u32
            }
            None => u32::MAX,
        }
    }

    pub fn ev(&mut self, h: u32) -> Option<&mut EvCb> {
        self.ensure();
        Self::index(h).and_then(|i| self.events.get_mut(i))
    }

    pub fn event_ready(&self, h: u32) -> bool {
        Self::index(h).and_then(|i| self.events.get(i)).is_some_and(|e| e.status == EV_READY)
    }

    /// DeliverEvent: interrupt-mode handlers to call; other enabled events
    /// become ready.
    pub fn deliver(&mut self, class: u32, spec: u32) -> Vec<u32> {
        let mut calls = Vec::new();
        for e in &mut self.events {
            if e.status != EV_ENABLED || e.class != class || e.spec != spec {
                continue;
            }
            if e.mode == EV_MODE_INTR {
                if e.func != 0 {
                    calls.push(e.func);
                }
            } else {
                e.status = EV_READY;
            }
        }
        calls
    }

    pub fn timer_irq_enabled(&self, n: u8) -> bool {
        self.timer_irq.get(n as usize).copied().unwrap_or(false)
    }

    /// Kernel pad handler: at each vblank, write each port's state into
    /// the `InitPAD` buffers (status 0x00 + ID + data, or 0xFF when
    /// nothing is connected).
    pub fn pad_vblank(&mut self, mem: &mut dyn Mem, host: &mut dyn Host) {
        let (Some(bufs), true) = (self.pad_bufs, self.pad_started) else { return };
        for (port, (addr, size)) in bufs.iter().enumerate() {
            if *addr == 0 || *size < 2 {
                continue;
            }
            let state = host.pad_state(port as u32);
            if state.is_empty() {
                mem.w8(*addr, 0xFF);
                continue;
            }
            let mut out = vec![0x00, if state.len() > 2 { 0x73 } else { 0x41 }];
            out.extend(&state);
            out.truncate(*size as usize);
            mem.write(*addr, &out);
        }
    }
}
