//! Kernel state behind the BIOS functions: event control blocks, the pad
//! buffers the kernel fills at vblank, open files, the heap, interrupt
//! handler queues (psx-spx "Kernel (BIOS)").

use crate::machine::Host;
use crate::{Machine, Sdk};
#[allow(unused_imports)]
use crate::IrqStep;
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
    /// `StartCARD2` ran (`StopCARD2` undoes it): the pad and card handler
    /// paces card operations.
    pub card_started: bool,
    /// psx-spx's `pad_enable_flag`: set by `InitPAD2`, and by
    /// `InitCARD2(pad_enable)`, which selects whether the pads are read
    /// with the cards.
    pub pad_enable: bool,
    /// `PAD_init2`'s `button_dest`: where `PAD_dr` stores its result; the
    /// handler calls it at each vblank when nonzero.
    pub pad_dest: u32,
    pub heap: (u32, u32),
    pub files: BTreeMap<u32, File>,
    /// Whether the kernel's handlers acknowledge what they handle: root
    /// counters 0-2 and vblank (`ChangeClearRCnt`), the pad handler's vblank
    /// (`ChangeClearPad`), the IRQ events' lines (`SetIrqAutoAck`).
    pub clear_rcnt: [u32; 4],
    pub clear_pad: u32,
    pub irq_auto_ack: [u32; 11],
    pub custom_exit: u32,
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

    /// The pad and card handler's pads: each port's state into the
    /// `InitPAD2` buffers (status 0x00 + ID + data, or 0xFF when nothing is
    /// connected), at most 22h bytes whatever the buffer's size; then
    /// `PAD_dr` when `PAD_init2` named a destination.
    pub fn pad_vblank(&mut self, mem: &mut dyn Mem, host: &mut dyn Host) {
        let (Some(bufs), true) = (self.pad_bufs, self.pad_enable) else { return };
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

    /// Every block, in order (the run report).
    pub fn events(&self) -> Vec<EvCb> {
        let n = self.mem.r32(k::EVCB_ENTRY + 4) / k::EVCB_SIZE;
        (0..n).filter_map(|i| self.evcb_at(EV_HANDLE | i)).map(|a| self.read_ev(a)).collect()
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

/// The kernel's own `SysEnqIntRP` elements, in its data (D68): root
/// counters 0-2 and vblank (0-3, priority 1) and the IRQ events (priority
/// 3), as OpenBIOS's `handlers/irq.c` enqueues them at boot, and the pad and
/// card handler (priority 2, `sio0/driver.c`), which `StartPAD2` and
/// `StartCARD2` enqueue. Their verifiers and handlers are kernel code here,
/// at addresses in the BIOS ROM's range ([`KERNEL_CODE`]), run as
/// [`IrqStep::Kernel`] steps in their places.
pub const KERNEL_ELEMENTS: u32 = 0x8000_7100;
pub const IRQ_ELEMENT: u32 = 4;
pub const SIO0_ELEMENT: u32 = 5;
const ELEMENTS: u32 = 6;
const KERNEL_CODE: u32 = 0xBFC0_8000;

pub fn kernel_element(i: u32) -> u32 {
    KERNEL_ELEMENTS + 16 * i
}

/// The IRQ events' order and classes (OpenBIOS's `IRQVerifier`, with the
/// retail BIOS's RTC1 for timer 2).
const IRQ_EVENTS: [(u32, u32); 11] = [
    (2, 0xF000_0003),
    (9, 0xF000_0009),
    (1, 0xF000_0002),
    (10, 0xF000_000A),
    (8, 0xF000_000B),
    (0, 0xF000_0001),
    (4, 0xF000_0005),
    (5, 0xF000_0006),
    (6, 0xF000_0006),
    (7, 0xF000_0008),
    (3, 0xF000_0004),
];

impl<S: Sdk> Machine<S> {
    /// The elements, the IRQ events at priority 3 and the root counters at
    /// 1, acknowledging what they handle (`enqueueIrqHandler(3)`,
    /// `enqueueRCntIrqs(1)`).
    pub fn lay_out_kernel_handlers(&mut self) {
        for i in 0..ELEMENTS {
            let e = kernel_element(i);
            for (o, v) in [(0, 0), (4, KERNEL_CODE + 8 * i + 4), (8, KERNEL_CODE + 8 * i), (12, 0)] {
                self.mem.w32(e + o, v);
            }
        }
        self.kernel.irq_auto_ack = [0; 11];
        self.enq_int_rp(3, kernel_element(IRQ_ELEMENT));
        for i in 0..4 {
            self.kernel.clear_rcnt[i] = 1;
            self.enq_int_rp(1, kernel_element(i as u32));
        }
    }

    /// The kernel element `h` is, if it is one.
    pub fn kernel_element_at(&self, h: u32) -> Option<u32> {
        let off = (h & 0x1FFF_FFFF).checked_sub(KERNEL_ELEMENTS & 0x1FFF_FFFF)?;
        (off % 16 == 0 && off / 16 < ELEMENTS).then_some(off / 16)
    }

    /// Kernel element `i`'s verifier, and its handler when that says so;
    /// whether the handler returned from the exception (the rest of it
    /// skipped).
    pub fn kernel_step(&mut self, i: u32) -> bool {
        let taken = |m: &Self, bit: u32| m.io.as_ref().is_some_and(|io| io.irq.stat & io.irq.mask & 1 << bit != 0);
        let ack = |m: &mut Self, bit: u32| {
            let _ = m.reg_write(psx_devices::irq::I_STAT, 4, !(1 << bit));
        };
        match i {
            0..=3 => {
                let bit = if i == 3 { psx_devices::irq::VBLANK } else { psx_devices::irq::TIMER0 + i };
                if !taken(self, bit) {
                    return false;
                }
                self.deliver_event(0xF200_0000 | i, 2);
                if self.kernel.clear_rcnt[i as usize] == 0 {
                    return false;
                }
                ack(self, bit);
                true
            }
            IRQ_ELEMENT => {
                for (bit, class) in IRQ_EVENTS {
                    if taken(self, bit) {
                        self.deliver_event(class, 0x1000);
                        if self.kernel.irq_auto_ack[bit as usize] != 0 {
                            ack(self, bit);
                        }
                    }
                }
                false
            }
            SIO0_ELEMENT => {
                let vblank = psx_devices::irq::VBLANK;
                if !taken(self, vblank) {
                    return false;
                }
                self.kernel.pad_vblank(&mut *self.mem, &mut *self.host);
                if self.kernel.clear_pad != 0 {
                    ack(self, vblank);
                }
                false
            }
            _ => false,
        }
    }

    /// `StartPAD2`/`StartCARD2` (OpenBIOS `startPad`, `startCard`): the pad
    /// and card handler at the head of priority 2, vblank acknowledged and
    /// unmasked, its acknowledge by the pad handler instead of RCntCNT3's.
    pub fn start_sio0(&mut self, card: bool) {
        let e = kernel_element(SIO0_ELEMENT);
        self.deq_int_rp(2, e);
        self.enq_int_rp(2, e);
        let vblank = 1 << psx_devices::irq::VBLANK;
        let _ = self.reg_write(psx_devices::irq::I_STAT, 4, !vblank);
        let mask = self.reg_read(psx_devices::irq::I_MASK, 4).unwrap_or(0);
        let _ = self.reg_write(psx_devices::irq::I_MASK, 4, mask | vblank);
        self.kernel.clear_pad = 1;
        self.kernel.clear_rcnt[3] = 0;
        self.kernel.card_started |= card;
    }

    /// `StopPAD2`/`StopCARD2`: RCntCNT3's acknowledge back, the handler out
    /// (for the pads and the cards both).
    pub fn stop_sio0(&mut self, card: bool) {
        self.kernel.clear_rcnt[3] = 1;
        self.deq_int_rp(2, kernel_element(SIO0_ELEMENT));
        if card {
            self.kernel.card_started = false;
        }
    }

    /// Whether the pad and card handler is in its chain.
    pub fn sio0_started(&self) -> bool {
        self.chain(2).contains(&kernel_element(SIO0_ELEMENT))
    }
}
