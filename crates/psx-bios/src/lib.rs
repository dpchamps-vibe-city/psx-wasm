//! The PlayStation BIOS as the SDK uses it (docs/design.md §5): the A0h,
//! B0h and C0h function tables and `syscall`, over kernel state the runtime
//! owns. Identical for every SDK version; no BIOS ROM is involved (D4).
//!
//! [`Machine`] is the state the BIOS and the SDK adapters share: guest
//! memory, the host, devices on the virtual timeline, kernel state, and the
//! queue of guest callbacks.

pub mod bios;
pub mod card;
pub mod io;
pub mod kernel;
pub mod machine;
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
    }

    impl Host for TestHost {
        fn log(&mut self, _level: u32, msg: &str) {
            self.logs.push(msg.to_string());
        }
        fn wait_vblank(&mut self, _frame: u64) {}
        fn disc_read(&mut self, _lba: u32, _out: &mut [u8; 2352]) -> bool {
            false
        }
        fn has_disc(&mut self) -> bool {
            false
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
        Machine::new(
            Box::new(VecMem::default()),
            Box::new(TestHost::default()),
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

    fn ret(f: Result<Flow<testing::NoSdk>, String>) -> u32 {
        match f.unwrap() {
            Flow::Return(v) => v as u32,
            Flow::Block { .. } | Flow::Call { .. } => panic!("blocked"),
        }
    }

    #[test]
    fn root_counter_events() {
        let mut m = machine();
        // OpenEvent(RCntCNT2, EvSpINT, EvMdNOINTR, 0); EnableEvent.
        let h = ret(bios::call(&mut m, 0xB0, 0x08, [0xF200_0002, 0x0002, 0x2000, 0]));
        assert_eq!(h, 0xF100_0000);
        ret(bios::call(&mut m, 0xB0, 0x0C, [h, 0, 0, 0]));
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x0B, [h, 0, 0, 0])), 0);
        m.timers[2].write_mode(0x58, 0);
        m.timers[2].target = 999;
        m.reschedule_timer(2);
        // Masked, the kernel's handler doesn't see it.
        m.io = Some(Default::default());
        m.run_until(1000);
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x0B, [h, 0, 0, 0])), 0);
        // StartRCnt(RCntCNT2) unmasks IRQ6.
        m.io_write(0x1F80_1074, 4, 1 << 6).unwrap();
        m.run_until(2000);
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x0B, [h, 0, 0, 0])), 1);
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x0B, [h, 0, 0, 0])), 0);
        // Interrupt-mode events queue their handler.
        let h2 = ret(bios::call(&mut m, 0xB0, 0x08, [0xF200_0003, 0x0002, 0x1000, 0x8001_2340]));
        ret(bios::call(&mut m, 0xB0, 0x0C, [h2, 0, 0, 0]));
        m.run_until(m.clock.video.vblank_at(1));
        assert_eq!(m.vblanks, 1);
        assert_eq!(m.calls.pop_front().map(|c| c.addr), Some(0x8001_2340));
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
        assert_eq!(m.kernel.ev(h).unwrap().status, kernel::EV_UNUSED);
        ret(bios::call(&mut m, 0xB0, 0x0D, [h, 0, 0, 0]));
        assert_eq!(m.kernel.ev(h).unwrap().status, kernel::EV_UNUSED);
        // The slot is free for the next OpenEvent.
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x08, [0xF200_0002, 0x0002, 0x2000, 0])), h);
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

    #[test]
    fn init_card_selects_the_pads() {
        let mut m = machine();
        let buf = 0x8001_0000;
        let filled = |m: &mut Machine<testing::NoSdk>| {
            m.mem.w8(buf, 0x5A);
            m.kernel.pad_vblank(&mut *m.mem, &mut *m.host);
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
    fn open_fails_without_a_descriptor() {
        let mut m = machine();
        for fd in 0..16 {
            m.kernel.files.insert(fd, kernel::File { lba: 0, size: 0, pos: 0 });
        }
        m.mem.write(0x8001_0000, b"cdrom:\\A.BIN;1\0");
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x32, [0x8001_0000, 1, 0, 0])), u32::MAX);
        assert_eq!(ret(bios::call(&mut m, 0xB0, 0x54, [0; 4])), card::E_NO_FDS);
    }

    #[test]
    fn reset_entry_int_drops_the_hook() {
        let mut m = machine();
        ret(bios::call(&mut m, 0xB0, 0x19, [0x8001_0000, 0, 0, 0]));
        assert_eq!(m.kernel.custom_exit, 0x8001_0000);
        ret(bios::call(&mut m, 0xB0, 0x18, [0; 4]));
        assert_eq!(m.kernel.custom_exit, 0);
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
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0x1B, [0x8001_0100, 0, 0, 0])), 3);
        m.mem.write(0x8001_0200, b"vam\0");
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0x17, [0x8001_0100, 0x8001_0200, 0, 0])) as i32, -1);
        ret(bios::call(&mut m, 0xA0, 0x2B, [0x8001_0300, 0x5A, 3, 0]));
        assert_eq!(m.mem.bytes(0x8001_0300, 4), vec![0x5A, 0x5A, 0x5A, 0]);
        assert!(bios::call(&mut m, 0xA0, 0x05, [0; 4]).is_err(), "ioctl is not implemented");
    }
}
