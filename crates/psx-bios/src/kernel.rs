//! Kernel state behind the BIOS functions: event control blocks, the pad
//! buffers the kernel fills at vblank, open files, the heap, interrupt
//! handler queues (psx-spx "Kernel (BIOS)").

use crate::machine::Host;
use crate::{Machine, Sdk};
use psxw_abi::kernel as k;
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
/// `PAD_init2`'s own buffers (B0:15h), in the kernel's data: psx-spx has
/// them "hidden within the BIOS variables region"; the address is ours.
pub const PAD_INIT2_BUFS: [u32; 2] = [0x8000_7000, 0x8000_7022];
/// What the kernel's pad handler stores at most per port, whatever the
/// buffer's size (psx-spx `InitPAD2`).
pub const PAD_BUF: u32 = 0x22;

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
    /// `InitPAD` buffers: (address, size) per port.
    pub pad_bufs: Option<[(u32, u32); 2]>,
    /// The pad and card handler is in the chain (`StartPAD2`/`StartCARD2`;
    /// `StopPAD2` and `StopCARD2` take it out for both).
    pub pad_started: bool,
    /// psx-spx's `pad_enable_flag`: set by `InitPAD2`, and by
    /// `InitCARD2(pad_enable)`, which selects whether the pads are read
    /// with the cards.
    pub pad_enable: bool,
    /// `PAD_init2`'s `button_dest`: where `PAD_dr` stores its result; the
    /// handler calls it at each vblank when nonzero.
    pub pad_dest: u32,
    pub heap: (u32, u32),
    pub files: BTreeMap<u32, File>,
    /// `ChangeClearRCnt` / `ChangeClearPad` flags.
    pub clear_rcnt: [u32; 4],
    pub clear_pad: u32,
    pub custom_exit: u32,
    /// `SysEnqIntRP` queues: priority → handler structs.
    pub int_handlers: BTreeMap<u32, Vec<u32>>,
    /// Memory cards and their files.
    pub cards: crate::card::Cards,
    /// Threads; their TCBs are in guest RAM.
    pub threads: crate::threads::Threads,
    /// The C library's random generator (A0h:2Fh/30h).
    pub rand: u32,
}

impl Kernel {
    /// `InitPAD2`: zero-fill the buffers and remember them; `PAD_dr` is no
    /// longer called by the handler.
    pub fn init_pad(&mut self, mem: &mut dyn Mem, bufs: [(u32, u32); 2]) {
        for (addr, size) in bufs {
            // At most RAM's size.
            mem.write(addr, &vec![0; size.min(0x20_0000) as usize]);
        }
        self.pad_bufs = Some(bufs);
        self.pad_dest = 0;
        self.pad_enable = true;
    }

    /// `PAD_init2(type, button_dest)`: only types 20000000h and 20000001h;
    /// `InitPAD2` and `StartPAD2` on the kernel's own buffers. Returns 2,
    /// or 0 for another type.
    pub fn pad_init2(&mut self, mem: &mut dyn Mem, ty: u32, dest: u32) -> u32 {
        if !matches!(ty, 0x2000_0000 | 0x2000_0001) {
            return 0;
        }
        let [b1, b2] = PAD_INIT2_BUFS;
        self.init_pad(mem, [(b1, PAD_BUF), (b2, PAD_BUF)]);
        self.pad_dest = dest;
        self.pad_started = true;
        2
    }

    /// `PAD_dr`: each port's buttons from `PAD_init2`'s buffers as a
    /// byte-swapped halfword (port 1 low), FFFFh unless the pad answered
    /// with ID 41h, or 23h (ORed with 07C7h, bits 6 and 7 cleared by the
    /// analogue inputs past 10h); stored at `button_dest` too, even when
    /// that's 0 (psx-spx).
    pub fn pad_dr(&self, mem: &mut dyn Mem) -> u32 {
        let mut v = [0xFFFFu16; 2];
        for (h, buf) in v.iter_mut().zip(PAD_INIT2_BUFS) {
            let b = mem.bytes(buf, 7);
            if b[0] != 0 || !matches!(b[1], 0x41 | 0x23) {
                continue;
            }
            *h = (b[2] as u16) << 8 | b[3] as u16;
            if b[1] == 0x23 {
                *h |= 0x7C7;
                if b[5] > 0x10 {
                    *h &= !0x40;
                }
                if b[6] > 0x10 {
                    *h &= !0x80;
                }
            }
        }
        let out = v[0] as u32 | (v[1] as u32) << 16;
        mem.w32(self.pad_dest, out);
        out
    }

    /// Kernel pad handler: at each vblank, write each port's state into
    /// the `InitPAD2` buffers (status 0x00 + ID + data, or 0xFF when
    /// nothing is connected), at most 22h bytes whatever the buffer's
    /// size; then `PAD_dr` when `PAD_init2` named a destination.
    pub fn pad_vblank(&mut self, mem: &mut dyn Mem, host: &mut dyn Host) {
        let (Some(bufs), true) = (self.pad_bufs, self.pad_started && self.pad_enable) else { return };
        for (port, (addr, _)) in bufs.iter().enumerate() {
            let state = host.pad_state(port as u32);
            if state.is_empty() {
                mem.w8(*addr, 0xFF);
                continue;
            }
            let mut out = vec![0x00, if state.len() > 2 { 0x73 } else { 0x41 }];
            out.extend(&state);
            out.truncate(PAD_BUF as usize);
            mem.write(*addr, &out);
        }
        if self.pad_dest != 0 {
            self.pad_dr(mem);
        }
    }
}

/// The event control blocks, in kernel RAM through the table of tables
/// (D68), as OpenBIOS's `events.c` keeps them.
impl<S: Sdk> Machine<S> {
    /// `n` blocks (SYSTEM.CNF `EVENT=`), all free.
    pub fn lay_out_events(&mut self, n: u32) {
        let n = n.clamp(1, k::MAX_EVENTS);
        self.mem.w32(k::EVCB_ENTRY, 0xA000_0000 | k::EVCBS);
        self.mem.w32(k::EVCB_ENTRY + 4, n * k::EVCB_SIZE);
        self.mem.write(k::EVCBS, &vec![0; (n * k::EVCB_SIZE) as usize]);
    }

    /// Handle `h`'s block; before the boot laid them out, the default
    /// count's.
    fn evcb(&mut self, h: u32) -> Option<u32> {
        if self.mem.r32(k::EVCB_ENTRY) == 0 {
            self.lay_out_events(EVENTS as u32);
        }
        self.evcb_at(h)
    }

    fn evcb_at(&self, h: u32) -> Option<u32> {
        let (base, size) = (self.mem.r32(k::EVCB_ENTRY), self.mem.r32(k::EVCB_ENTRY + 4));
        let i = h & 0xFFFF;
        (base != 0 && h & 0xFFFF_0000 == EV_HANDLE && i < size / k::EVCB_SIZE).then_some(base + i * k::EVCB_SIZE)
    }

    fn read_ev(&self, a: u32) -> EvCb {
        let r = |o: u32| self.mem.r32(a + o);
        EvCb { class: r(0), status: r(4), spec: r(8), mode: r(0xC), func: r(0x10) }
    }

    /// Event `h`'s block, if `h` names one.
    pub fn ev(&mut self, h: u32) -> Option<EvCb> {
        self.evcb(h).map(|a| self.read_ev(a))
    }

    pub fn set_ev_status(&mut self, h: u32, status: u32) {
        if let Some(a) = self.evcb(h) {
            self.mem.w32(a + 4, status);
        }
    }

    /// `OpenEvent`: the first free block, disabled; its handle, or -1.
    pub fn open_event(&mut self, class: u32, spec: u32, mode: u32, func: u32) -> u32 {
        self.evcb(EV_HANDLE);
        let n = self.mem.r32(k::EVCB_ENTRY + 4) / k::EVCB_SIZE;
        for i in 0..n {
            let a = self.evcb_at(EV_HANDLE | i).expect("in range");
            if self.mem.r32(a + 4) == EV_UNUSED {
                for (o, v) in [(0, class), (4, EV_DISABLED), (8, spec), (0xC, mode), (0x10, func)] {
                    self.mem.w32(a + o, v);
                }
                return EV_HANDLE | i;
            }
        }
        u32::MAX
    }

    pub fn event_ready(&self, h: u32) -> bool {
        self.evcb_at(h).is_some_and(|a| self.mem.r32(a + 4) == EV_READY)
    }

    /// DeliverEvent: interrupt-mode handlers to call; other enabled events
    /// become ready.
    pub(crate) fn event_calls(&mut self, class: u32, spec: u32) -> Vec<u32> {
        let mut calls = Vec::new();
        let n = self.evcb(EV_HANDLE).map_or(0, |_| self.mem.r32(k::EVCB_ENTRY + 4) / k::EVCB_SIZE);
        for i in 0..n {
            let a = self.evcb_at(EV_HANDLE | i).expect("in range");
            let e = self.read_ev(a);
            if e.status != EV_ENABLED || e.class != class || e.spec != spec {
                continue;
            }
            if e.mode == EV_MODE_INTR {
                if e.func != 0 {
                    calls.push(e.func);
                }
            } else {
                self.mem.w32(a + 4, EV_READY);
            }
        }
        calls
    }
}
