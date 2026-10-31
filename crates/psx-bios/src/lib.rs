//! The PlayStation BIOS as the SDK uses it (docs/design.md §5): the A0h,
//! B0h and C0h function tables and `syscall`, over kernel state the runtime
//! owns. Identical for every SDK version; no BIOS ROM is involved (D4).
//!
//! [`Machine`] is the state the BIOS and the SDK adapters share: guest
//! memory, the host, devices on the virtual timeline, kernel state, and the
//! queue of guest callbacks.

pub mod bios;
pub mod card;
pub mod cdrom;
pub mod files;
pub mod io;
pub mod kernel;
pub mod libc;
pub mod loader;
pub mod machine;
pub mod tables;
pub mod threads;

pub use machine::{Event, Flow, GuestCall, Host, IrqStep, Machine, Regs, Sdk, StandIn, Wait};

#[cfg(test)]
pub mod testing {
    //! A machine over a `VecMem` and a recording host.
    use super::*;
    use psx_devices::mem::VecMem;
    use psxw_abi::BootOptions;

    #[derive(Default)]
    pub struct TestHost {
        pub logs: Vec<String>,
        /// Raw sectors from LBA 0.
        pub disc: Vec<[u8; 2352]>,
    }

    impl Host for TestHost {
        fn log(&mut self, _level: u32, msg: &str) {
            self.logs.push(msg.to_string());
        }
        fn wait_vblank(&mut self, _frame: u64) {}
        fn disc_read(&mut self, lba: u32, out: &mut [u8; 2352]) -> bool {
            self.disc.get(lba as usize).map(|s| *out = *s).is_some()
        }
        fn has_disc(&mut self) -> bool {
            !self.disc.is_empty()
        }
        fn pad_state(&mut self, _port: u32) -> Vec<u8> {
            vec![0xFF, 0xFF]
        }
        fn storage_read(&mut self, _name: &str) -> Option<Vec<u8>> {
            None
        }
        fn storage_write(&mut self, _name: &str, _data: &[u8]) -> bool {
            false
        }
        fn report(&mut self, _json: &str) {}
    }

    pub struct TestRegs {
        pub sp: u32,
        pub gp: u32,
    }

    impl Regs for TestRegs {
        fn sp(&self) -> u32 {
            self.sp
        }
        fn set_sp(&mut self, v: u32) {
            self.sp = v;
        }
        fn gp(&self) -> u32 {
            self.gp
        }
        fn set_gp(&mut self, v: u32) {
            self.gp = v;
        }
    }

    pub struct NoSdk;

    impl Sdk for NoSdk {
        fn on_event(_m: &mut Machine<Self>, _kind: u32, _data: u32) {}
        fn ready(_m: &Machine<Self>, _cond: u32) -> bool {
            true
        }
        fn on_vblank(_m: &mut Machine<Self>) {}
    }

    pub fn machine() -> Machine<NoSdk> {
        machine_with_disc(Vec::new())
    }

    pub fn machine_with_disc(disc: Vec<[u8; 2352]>) -> Machine<NoSdk> {
        Machine::new(
            Box::new(VecMem::default()),
            Box::new(TestHost { disc, ..Default::default() }),
            Box::new(TestRegs { sp: 0x801F_F000, gp: 0 }),
            BootOptions::default(),
            NoSdk,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::testing::machine;
    use super::*;
    use crate::threads;

    fn ret(f: Result<Flow<testing::NoSdk>, String>) -> u32 {
        match f.unwrap() {
            Flow::Return(v) => v as u32,
            Flow::Block { .. } | Flow::Call { .. } => panic!("blocked"),
        }
    }

    /// Native chain functions run; ordinary guest callbacks are recorded.
    fn invoke(m: &mut Machine<testing::NoSdk>, call: GuestCall) -> u32 {
        if call.addr == psxw_abi::BIOS_RETURN_FROM_EXCEPTION {
            m.rfe = true;
            return 0;
        }
        let Some(mut flow) = m.kernel_call(call.addr, call.args) else {
            m.calls.push_back(call);
            return 0;
        };
        loop {
            match flow {
                Flow::Return(v) => return v as u32,
                Flow::Call { call, then } => {
                    let v = invoke(m, call);
                    if m.rfe { return 0; }
                    flow = then(m, v as u64);
                }
                Flow::Block { .. } => panic!("kernel handler blocked"),
            }
        }
    }

    fn take(m: &mut Machine<testing::NoSdk>) {
        while let Some(s) = m.irq_calls.pop_front() {
            if let IrqStep::Chain(call, handler) = s {
                m.in_irq = true;
                m.rfe = false;
                let v = invoke(m, call);
                if let Some(addr) = handler.filter(|_| v != 0 && !m.rfe) {
                    invoke(m, GuestCall { addr, args: [v, 0, 0, 0] });
                }
                if m.rfe {
                    while m.irq_calls.pop_front().is_some_and(|s| s != IrqStep::Exit) {}
                }
                m.rfe = false;
                m.in_irq = false;
            }
        }
    }

    #[test]
    fn root_counter_events() {
        let mut m = machine();
        // OpenEvent(RCntCNT2, EvSpINT, EvMdNOINTR, 0); EnableEvent.
        let h = ret(bios::call(&mut m, 0xB0, 0x08, [0xF200_0002, 0x0002, 0x2000, 0]));
        assert_eq!(h, 0xF100_0000);
        // Its block in kernel RAM, through the table of tables (D68).
        let evcb = m.mem.r32(psxw_abi::kernel::EVCB_ENTRY);
        assert_eq!(evcb, 0xA000_0000 | psxw_abi::kernel::EVCBS);
        assert_eq!(m.mem.bytes(evcb, 16), [2, 0, 0, 0xF2, 0, 0x10, 0, 0, 2, 0, 0, 0, 0, 0x20, 0, 0]);
        ret(bios::call(&mut m, 0xB0, 0x0C, [h, 0, 0, 0]));
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x0B, [h, 0, 0, 0])), 0);
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x02, [0xF200_0002, 999, 0x1001, 0])), 1);
        assert_eq!(m.timers[2].mode, 0x58);
        // Masked, the kernel's handler doesn't see it.
        m.io = Some(Default::default());
        m.run_until(1000);
        take(&mut m);
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x0B, [h, 0, 0, 0])), 0);
        // enable_timer_irq unmasks IRQ6, including the pending interrupt.
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x04, [0xF200_0002, 0, 0, 0])), 1);
        take(&mut m);
        assert_eq!(m.ev(h).unwrap().status, kernel::EV_READY);
        ret(bios::call(&mut m, 0xB0, 0x20, [0xF200_0002, 0x0002, 0, 0]));
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x0B, [h, 0, 0, 0])), 0);
        m.run_until(2000);
        take(&mut m);
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x0B, [h, 0, 0, 0])), 1);
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x06, [2, 0, 0, 0])), 1);
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x03, [2, 0, 0, 0])), 0);
        assert_eq!(m.io.as_ref().unwrap().irq.stat & 1 << 6, 0, "acknowledged");
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x0B, [h, 0, 0, 0])), 0);
        // Interrupt-mode events queue their handler; RCntCNT3 once vblank
        // is unmasked (StartRCnt(RCntCNT3)).
        let h2 = ret(bios::call(&mut m, 0xB0, 0x08, [0xF200_0003, 0x0002, 0x1000, 0x8001_2340]));
        ret(bios::call(&mut m, 0xB0, 0x0C, [h2, 0, 0, 0]));
        m.run_until(m.clock.video.vblank_at(1));
        take(&mut m);
        assert_eq!(m.vblanks, 1);
        assert!(m.calls.is_empty(), "vblank masked");
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x04, [3, 0, 0, 0])), 0);
        m.run_until(m.clock.video.vblank_at(2));
        take(&mut m);
        assert_eq!(m.calls.pop_front().map(|c| c.addr), Some(0x8001_2340));
    }

    #[test]
    fn reinstall_native_irq_services() {
        let mut m = machine();
        m.io = Some(Default::default());
        m.run_until(1000);
        // Dequeue the defaults, preserving the elements and unrelated state.
        for i in 0..4 {
            ret(bios::call(&mut m, 0xC0, 3, [1, kernel::kernel_element(i), 0, 0]));
        }
        ret(bios::call(&mut m, 0xC0, 3, [3, kernel::kernel_element(kernel::IRQ_ELEMENT), 0, 0]));
        let root = kernel::kernel_element(3);
        m.mem.w32(root + 4, 0x8001_2340);
        m.kernel.clear_rcnt = [0; 4];
        m.kernel.clear_pad = 7;
        m.kernel.irq_auto_ack = [1; 11];
        m.io.as_mut().unwrap().irq.mask = 0x7FF;
        m.io.as_mut().unwrap().irq.stat = 0x71;
        ret(bios::call(&mut m, 0xB0, 2, [0, 50, 0x1000, 0]));
        ret(bios::call(&mut m, 0xC0, 0, [2, 0, 0, 0]));
        ret(bios::call(&mut m, 0xC0, 0xC, [3, 0, 0, 0]));
        assert_eq!(m.chain(2), [3, 2, 1, 0].map(kernel::kernel_element));
        assert_eq!(m.chain(3), [kernel::kernel_element(kernel::IRQ_ELEMENT)]);
        assert!(m.chain(0).is_empty() && m.chain(1).is_empty());
        assert_eq!(m.mem.r32(root + 4), 0x8001_2340);
        assert_eq!(m.kernel.clear_rcnt, [1; 4]);
        assert_eq!(m.kernel.clear_pad, 7);
        assert_eq!(m.kernel.irq_auto_ack, [0; 11]);
        assert_eq!(m.io.as_ref().unwrap().irq.mask, 0x7FF & !0x71);
        assert_eq!(m.io.as_ref().unwrap().irq.stat, 0x71, "masking is not acknowledgement");
        for (i, timer) in m.timers.iter().enumerate() {
            assert_eq!((timer.mode, timer.target, timer.value(i, m.clock.video, m.timer_now(i))), (0, 0, 0));
        }
        assert!(m.clock.pending(|e| matches!(e, Event::Timer(_))).is_none());
    }

    #[test]
    fn critical_sections_hold_interrupts() {
        let mut m = machine();
        assert_eq!(bios::syscall(&mut m, [1, 0, 0, 0]), Ok(1));
        assert_eq!(bios::syscall(&mut m, [1, 0, 0, 0]), Ok(0));
        m.run_until(m.clock.video.vblank_at(2));
        assert_eq!(m.vblanks, 0);
        bios::syscall(&mut m, [2, 0, 0, 0]).unwrap();
        m.run_until(m.clock.now);
        assert_eq!(m.vblanks, 2);
    }

    #[test]
    fn closed_events_stay_closed() {
        let mut m = machine();
        let h = ret(bios::call(&mut m, 0xB0, 0x08, [0xF200_0002, 0x0002, 0x2000, 0]));
        ret(bios::call(&mut m, 0xB0, 0x09, [h, 0, 0, 0]));
        ret(bios::call(&mut m, 0xB0, 0x0C, [h, 0, 0, 0]));
        assert_eq!(m.ev(h).unwrap().status, kernel::EV_UNUSED);
        ret(bios::call(&mut m, 0xB0, 0x0D, [h, 0, 0, 0]));
        assert_eq!(m.ev(h).unwrap().status, kernel::EV_UNUSED);
        // The slot is free for the next OpenEvent.
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x08, [0xF200_0002, 0x0002, 0x2000, 0])), h);
    }

    #[test]
    fn kernel_memory_follows_the_event_blocks() {
        let mut m = machine();
        let a = ret(bios::call(&mut m, 0xB0, 0x00, [0x10, 0, 0, 0]));
        assert_eq!(a, 0xA000_F9D4);
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x00, [0x1000, 0, 0, 0])), 0, "too big");
        ret(bios::call(&mut m, 0xB0, 0x01, [a, 0, 0, 0]));
        assert_eq!(m.mem.r32(a - 4), 0x10 | 1);
        assert_eq!(m.kernel.heap, (0, 0), "application heap untouched");
        let mut m = machine();
        m.lay_out_events(psxw_abi::kernel::MAX_EVENTS);
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x00, [0x100, 0, 0, 0])), 0, "no wrap past kernel RAM");
    }

    #[test]
    fn open_fails_without_a_descriptor() {
        let mut m = machine();
        for fd in 0..16 {
            m.mem.w32(files::fcb_at(fd), 1);
        }
        m.mem.write(0x8001_0000, b"cdrom:\\A.BIN;1\0");
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x32, [0x8001_0000, 1, 0, 0])), u32::MAX);
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x54, [0; 4])), files::E_NO_FDS);
    }

    #[test]
    fn pad_init2_and_pad_dr() {
        let mut m = machine();
        let dest = 0x8001_0000;
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x15, [0x1000_0001, dest, 0, 0])), 0);
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x15, [0x2000_0000, dest, 0, 0])), 2);
        m.kernel.pad_vblank(&mut *m.mem, &mut *m.host);
        assert_eq!(m.mem.bytes(kernel::PAD_INIT2_BUFS[0], 4), [0x00, 0x41, 0xFF, 0xFF]);
        assert_eq!(m.mem.r32(dest), 0xFFFF_FFFF);
        // A digital pad on port 1; on port 2 a 23h device, its first
        // analogue input past 10h.
        m.mem.write(kernel::PAD_INIT2_BUFS[0], &[0x00, 0x41, 0x12, 0x34]);
        m.mem.write(kernel::PAD_INIT2_BUFS[1], &[0x00, 0x23, 0xFF, 0xFF, 0, 0x11, 0x05]);
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x16, [0; 4])), 0xFFBF_1234);
        assert_eq!(m.mem.r32(dest), 0xFFBF_1234);
    }

    /// The A0h GPU calls the device libraries use: GP0/GP1 words reach the
    /// GPU core, GPUSTAT comes back from it, and GP1's register address is
    /// what `gpu_abort_dma` returns.
    #[test]
    fn gpu_control_calls_reach_the_core() {
        let mut m = machine();
        m.io = Some(Default::default());
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0x49, [0, 0, 0, 0])), 0, "GPU_cw: a GP0 NOP");
        assert_eq!(m.io.as_ref().unwrap().gp0_words, 1);
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0x48, [0x0300_0001, 0, 0, 0])), 0, "SendGP1Command");
        assert!(m.io.as_ref().unwrap().gpu.display().off);
        let status = m.io.as_ref().unwrap().gpu.status(m.clock.now, m.clock.video);
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0x4D, [0, 0, 0, 0])), status, "GetGPUStatus");
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0x4E, [0, 0, 0, 0])), 0, "gpu_sync");
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0x4C, [0, 0, 0, 0])), 0x1F80_1814, "gpu_abort_dma");
        assert_eq!(m.io.as_ref().unwrap().gpu.display().dma, 0);
    }

    /// SendGP1Command (A0h:48h) sets root counter 0's dot clock to the
    /// display's width, as a GP1 write on the bus does.
    #[test]
    fn gpu_gp1_command_sets_the_dot_clock() {
        let mut m = machine();
        m.io = Some(Default::default());
        m.timers[0].write_mode(0x100, 0);
        let v = m.clock.video;
        m.clock.now = v.dot_at(1000);
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0x48, [0x0800_0001, 0, 0, 0])), 0);
        assert_eq!(m.timers[0].value(0, v, m.clock.now), 100);
    }

    /// GPU_dw and gpu_send_dma send GP0(A0h) and the image words, in order,
    /// and leave the BIOS's arguments and result on the caller's stack;
    /// send_gpu_linked_list walks the list through channel 2.
    #[test]
    fn gpu_image_transfers_reach_the_core() {
        use psxw_abi::render::{self, Command};
        let mut m = machine();
        m.io = Some(Default::default());
        let (src, sp) = (0x8001_0000, m.regs.sp());
        m.mem.write(src, &[0x01, 0x00, 0x02, 0x00, 0x03, 0x00, 0x04, 0x00]);
        m.mem.w32(sp + 16, src);
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0x46, [0x100, 0x20, 2, 2])), src + 8, "GPU_dw");
        let io = m.io.as_mut().unwrap();
        assert_eq!(io.gp0_words, 5, "header and two pixel words");
        let words = io.gpu.take_commands();
        assert!(matches!(
            render::commands(&words).next(),
            Some(Ok(Command::Upload { mask: 0, pixels, .. })) if pixels == [0x0002_0001, 0x0004_0003]
        ));
        assert_eq!(m.mem.bytes(sp + 4, 12), [0x20, 0, 0, 0, 2, 0, 0, 0, 2, 0, 0, 0], "arguments left on the stack");
        assert_eq!(m.mem.r32(sp + 16), src + 8, "end address left on the stack");

        // gpu_send_dma: 8x8 pixels, 32 words in 2 blocks of 16.
        let (dst, n) = (0x8002_0000, 32);
        for i in 0..n {
            m.mem.w32(dst + 4 * i as u32, i);
        }
        m.mem.w32(sp + 16, dst);
        let before = m.io.as_ref().unwrap().gp0_words;
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0x47, [0x40, 0x80, 8, 8])), 0x1F80_1810, "gpu_send_dma");
        let io = m.io.as_mut().unwrap();
        assert_eq!(io.gp0_words - before, 35);
        assert_eq!(io.dma.madr[2], (dst & 0x1F_FFFC) + 4 * n, "the channel ran to the end of the image");
        assert_eq!(io.dma.chcr[2] & psx_devices::dma::CHCR_BUSY, 0);
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0x4E, [0, 0, 0, 0])), 0);
        assert_eq!(m.io.as_ref().unwrap().gpu.display().dma, 0, "gpu_sync turns DMA direction off");

        // send_gpu_linked_list: two words, end bit set.
        let list = 0x8003_0000;
        m.mem.w32(list, 2 << 24 | 0x80_0000);
        m.mem.w32(list + 4, 0);
        m.mem.w32(list + 8, 0);
        let before = m.io.as_ref().unwrap().gp0_words;
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0x4B, [list, 0, 0, 0])), 0, "send_gpu_linked_list");
        let io = m.io.as_ref().unwrap();
        assert_eq!(io.gp0_words - before, 2);
        assert_eq!(io.dma.madr[2], 0x00FF_FFFF);
    }

    #[test]
    fn init_card_selects_the_pads() {
        let mut m = machine();
        m.io = Some(Default::default());
        let buf = 0x8001_0000;
        // A vblank taken: the pad and card handler, if in its chain.
        let filled = |m: &mut Machine<testing::NoSdk>| {
            m.mem.w8(buf, 0x5A);
            m.raise_irq(psx_devices::irq::VBLANK);
            take(m);
            m.mem.r8(buf) == 0
        };
        ret(bios::call(&mut m, 0xB0, 0x12, [buf, 0x22, buf + 0x22, 0x22]));
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x4A, [0; 4])), 0);
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x4B, [0; 4])), 1);
        assert!(!filled(&mut m), "InitCARD2(0) leaves the pads out");
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x4A, [1, 0, 0, 0])), 1, "initialised before");
        assert!(filled(&mut m));
        ret(bios::call(&mut m, 0xB0, 0x4C, [0; 4]));
        assert!(!filled(&mut m), "StopCARD2 stops the pads too");
    }

    #[test]
    fn reset_entry_int_drops_the_hook() {
        let mut m = machine();
        ret(bios::call(&mut m, 0xB0, 0x19, [0x8001_0000, 0, 0, 0]));
        assert_eq!(m.kernel.custom_exit, 0x8001_0000);
        let d = ret(bios::call(&mut m, 0xB0, 0x18, [0; 4]));
        assert_eq!(m.kernel.custom_exit, 0);
        assert_eq!(d, kernel::DEFAULT_EXIT);
        assert_eq!((m.mem.r32(d), m.mem.r32(d + 4)), (tables::stand_in(0xB0, 0x17), 0x8000_6FFC));
        assert_eq!(m.mem.bytes(d + 8, 0x28), vec![0; 0x28]);
        for alias in [d, d & 0x1FFF_FFFF, (d & 0x1FFF_FFFF) | 0x8000_0000] {
            ret(bios::call(&mut m, 0xB0, 0x19, [0x8001_0000, 0, 0, 0]));
            ret(bios::call(&mut m, 0xB0, 0x19, [alias, 0, 0, 0]));
            assert_eq!(m.kernel.custom_exit, 0);
        }
    }

    #[test]
    fn unknown_syscalls_deliver_the_exception_event() {
        let mut m = machine();
        m.lay_out_events(16);
        let h = ret(bios::call(&mut m, 0xB0, 0x08, [0xF000_0010, 0x4000, 0x2000, 0]));
        ret(bios::call(&mut m, 0xB0, 0x0C, [h, 0, 0, 0]));
        for n in [4, 7, u32::MAX] {
            assert_eq!(bios::syscall(&mut m, [n, 0, 0, 0]), Ok(0));
            assert_eq!(ret(bios::call(&mut m, 0xB0, 0x0B, [h, 0, 0, 0])), 1);
        }
    }

    /// The kernel/configuration family: RAM size and system information,
    /// the configuration the boot and `SetConf` leave, its free slots, the
    /// kernel memory region, and the entries that stay explicitly blocked.
    #[test]
    fn kernel_configuration_lifecycle() {
        use psxw_abi::kernel as k;
        let mut m = machine();
        // SetMem: only retail's 2 and 8 MB change the guest-visible size.
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0x9F, [8, 0, 0, 0])), 0);
        assert_eq!(m.mem.r32(k::RAM_SIZE), 8);
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0xB4, [5, 0, 0, 0])), 8192);
        ret(bios::call(&mut m, 0xA0, 0x9F, [3, 0, 0, 0]));
        assert_eq!(m.mem.r32(k::RAM_SIZE), 8, "an unsupported size is ignored");
        ret(bios::call(&mut m, 0xA0, 0x9F, [2, 0, 0, 0]));
        // GetSystemInfo: kernel/console facts; identity metadata is not
        // invented. The function stays partial while 0 and 2 are unsupported.
        for (index, value) in [(1, 3), (3, 0), (4, 0), (5, 2048), (6, 0), (7, 1024), (8, 0), (9, 512), (10, 0), (11, 0), (12, 1), (13, 1), (14, 1), (15, 0)] {
            assert_eq!(ret(bios::call(&mut m, 0xA0, 0xB4, [index, 0, 0, 0])), value, "GetSystemInfo({index:#x})");
        }
        for index in [0, 2] {
            assert!(bios::call(&mut m, 0xA0, 0xB4, [index, 0, 0, 0]).is_err(), "GetSystemInfo({index:#x}) needs identity");
        }
        // Retail's zero-return entries.
        for f in [0x0E, 0x0F, 0x10, 0x11, 0x14] {
            assert_eq!(ret(bios::call(&mut m, 0xC0, f, [0; 4])), 0);
            assert!(bios::implemented(0xC0, f));
        }
        // GetConf reports the boot's counts and initial stack top.
        m.regs.set_sp(0x801F_EE00);
        m.lay_out_threads(4);
        let (events, tcbs, stack) = (0x8001_0000, 0x8001_0004, 0x8001_0008);
        ret(bios::call(&mut m, 0xA0, 0x9D, [events, tcbs, stack, 0]));
        assert_eq!((m.mem.r32(events), m.mem.r32(tcbs), m.mem.r32(stack)), (kernel::EVENTS as u32, 4, 0x801F_EE00));
        ret(bios::call(&mut m, 0xA0, 0x9D, [0, tcbs, stack, 0]));
        assert_eq!(m.mem.r32(0), kernel::EVENTS as u32, "guest address zero is mapped");
        // C(04h)/C(05h) name the first free block; Open/Close update them.
        assert_eq!(ret(bios::call(&mut m, 0xC0, 0x04, [0; 4])), 0);
        let ev = ret(bios::call(&mut m, 0xB0, 0x08, [0xF200_0002, 0x0002, 0x2000, 0]));
        assert_eq!(ret(bios::call(&mut m, 0xC0, 0x04, [0; 4])), 1);
        ret(bios::call(&mut m, 0xB0, 0x09, [ev, 0, 0, 0]));
        assert_eq!(ret(bios::call(&mut m, 0xC0, 0x04, [0; 4])), 0);
        assert_eq!(ret(bios::call(&mut m, 0xC0, 0x05, [0; 4])), 1, "TCB 0 is the main thread");
        let th = ret(bios::call(&mut m, 0xB0, 0x0E, [0x8001_0000, 0x801F_E000, 0, 0]));
        assert_eq!(ret(bios::call(&mut m, 0xC0, 0x05, [0; 4])), 2);
        ret(bios::call(&mut m, 0xB0, 0x0F, [th, 0, 0, 0]));
        assert_eq!(ret(bios::call(&mut m, 0xC0, 0x05, [0; 4])), 1);
        // SetConf resets the blocks, chains and kernel memory, keeping the
        // main thread and reporting the new configuration.
        let stale = ret(bios::call(&mut m, 0xB0, 0x08, [0xF200_0002, 0x0002, 0x2000, 0]));
        for prio in 0..4 {
            m.enq_int_rp(prio, 0x8002_0000 + 4 * prio);
        }
        let default_chains = |m: &Machine<testing::NoSdk>| {
            let roots: Vec<u32> = (0..4).rev().map(kernel::kernel_element).collect();
            (m.chain(0), m.chain(1), m.chain(2), m.chain(3), roots)
        };
        let check_chains = |m: &Machine<testing::NoSdk>| {
            let (p0, p1, p2, p3, roots) = default_chains(m);
            assert!(p0.is_empty(), "priority 0 keeps no guest handler");
            assert_eq!(p1, roots, "priority 1 is the native root counters");
            assert!(p2.is_empty(), "priority 2 keeps no guest handler");
            assert_eq!(p3, vec![kernel::kernel_element(kernel::IRQ_ELEMENT)]);
            // Exact links: no trailing guest element and no self-cycle.
            for (i, &e) in roots.iter().enumerate() {
                let next = roots.get(i + 1).copied().unwrap_or(0);
                assert_eq!(m.mem.r32(e), next, "root-counter element {i} link");
            }
            assert_eq!(m.mem.r32(kernel::kernel_element(kernel::IRQ_ELEMENT)), 0);
        };
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0x9C, [8, 2, 0x801F_EC00, 0])), 0);
        check_chains(&m);
        assert_eq!(m.ev(stale).unwrap().status, kernel::EV_UNUSED, "old events are gone");
        // Repeating SetConf must not append or self-link the native chains.
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0x9C, [8, 2, 0x801F_EC00, 0])), 0);
        check_chains(&m);
        ret(bios::call(&mut m, 0xA0, 0x9D, [events, tcbs, stack, 0]));
        assert_eq!((m.mem.r32(events), m.mem.r32(tcbs), m.mem.r32(stack)), (8, 2, 0x801F_EC00));
        assert_eq!(m.kernel.kheap, (0, 0));
        assert_eq!(m.regs.sp(), 0x801F_EE00, "SetConf changes the configured top, not the running SP");
        assert_eq!(m.mem.r32(threads::tcb(0) + k::TCB_STATUS), k::TCB_USED);
        assert_eq!(ret(bios::call(&mut m, 0xC0, 0x05, [0; 4])), 1);
        let heap_start = 0xA000_0000 | (k::EVCBS + 8 * k::EVCB_SIZE);
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x00, [0x10, 0, 0, 0])), heap_start + 4);
        // A live non-main thread cannot be reset by the native runtime.
        ret(bios::call(&mut m, 0xB0, 0x0E, [0x8001_0000, 0x801F_E000, 0, 0]));
        m.enter_thread(1);
        assert!(bios::call(&mut m, 0xA0, 0x9C, [4, 3, 0, 0]).is_err());
        ret(bios::call(&mut m, 0xA0, 0x9D, [events, tcbs, stack, 0]));
        assert_eq!((m.mem.r32(events), m.mem.r32(tcbs)), (8, 2), "a refused reset changes nothing");
        m.enter_thread(0);
        // SysInitMemory reinitialises the allocator in the requested region,
        // including retail's default kernel range.
        assert_eq!(ret(bios::call(&mut m, 0xC0, 0x08, [0xA000_2000, 0x1000, 0, 0])), 0);
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x00, [0x10, 0, 0, 0])), 0xA000_2004);
        assert_eq!(ret(bios::call(&mut m, 0xC0, 0x08, [0xA000_E000, 0x2000, 0, 0])), 0);
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x00, [0x10, 0, 0, 0])), 0xA000_E004);
        assert!(bios::call(&mut m, 0xC0, 0x08, [0, 0x1000, 0, 0]).is_err());
        // Disputed/undocumented entries are explicit errors, not no-ops.
        assert!(bios::call(&mut m, 0xC0, 0x09, [0; 4]).is_err(), "SysInitKernelVariables stays blocked");
        assert!(bios::call(&mut m, 0xA0, 0x52, [0; 4]).is_err(), "A(52h) conflict stays unresolved");
    }

    #[test]
    fn strings_and_printf() {
        let mut m = machine();
        m.mem.write(0x8001_0000, b"%s=%04x %d%%\0");
        m.mem.write(0x8001_0100, b"val\0");
        let text = bios::printf(&mut m, [0x8001_0000, 0x8001_0100, 0xBEEF, (-5i32) as u32]);
        assert_eq!(text, "val=beef -5%");
        // Sign before zero padding; +, space, # and %n (psx-spx A(3Fh)).
        m.mem.write(0x8001_0000, b"%05d|%+d|% d|%#x|%#o|%-4u|%.3d%n\0");
        for (k, v) in [0x0A, 8, 8, 3, 0x8001_0200u32].iter().enumerate() {
            m.mem.w32(0x801F_F010 + 4 * k as u32, *v);
        }
        let a = [0x8001_0000, (-5i32) as u32, 7, 7];
        assert_eq!(bios::printf(&mut m, a), "-0005|+7| 7|0xa|010|8   |003");
        assert_eq!(m.mem.r32(0x8001_0200), 28);
        // Each conversion finishes before the next: %s sees %n's write.
        m.mem.write(0x8001_0000, b"%n%s\0");
        m.mem.write(0x8001_0200, b"old\0");
        assert_eq!(bios::printf(&mut m, [0x8001_0000, 0x8001_0200, 0x8001_0200, 0]), "");
        assert_eq!(m.mem.r32(0x8001_0200), 0);
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0x1B, [0x8001_0100, 0, 0, 0])), 3);
        m.mem.write(0x8001_0200, b"vam\0");
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0x17, [0x8001_0100, 0x8001_0200, 0, 0])) as i32, -1);
        ret(bios::call(&mut m, 0xA0, 0x2B, [0x8001_0300, 0x5A, 3, 0]));
        assert_eq!(m.mem.bytes(0x8001_0300, 4), vec![0x5A, 0x5A, 0x5A, 0]);
        assert!(bios::call(&mut m, 0xC0, 0x16, [0; 4]).is_err(), "_cdevscan is not implemented");
    }
}
