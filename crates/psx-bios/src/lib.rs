//! The PlayStation BIOS as the SDK uses it (docs/design.md §5): the A0h,
//! B0h and C0h function tables and `syscall`, over kernel state the runtime
//! owns. Identical for every SDK version; no BIOS ROM is involved (D4).
//!
//! [`Machine`] is the state the BIOS and the SDK adapters share: guest
//! memory, the host, devices on the virtual timeline, kernel state, and the
//! queue of guest callbacks.

pub mod bios;
pub mod io;
pub mod kernel;
pub mod machine;

pub use machine::{Event, Flow, GuestCall, Host, Machine, Regs, Sdk, Wait};

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
            Flow::Block { .. } => panic!("blocked"),
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
        m.kernel.timer_irq[2] = true;
        m.reschedule_timer(2);
        m.run_until(1000);
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
    fn strings_and_printf() {
        let mut m = machine();
        m.mem.write(0x8001_0000, b"%s=%04x %d%%\0");
        m.mem.write(0x8001_0100, b"val\0");
        let text = bios::printf(&m, [0x8001_0000, 0x8001_0100, 0xBEEF, (-5i32) as u32]);
        assert_eq!(text, "val=beef -5%");
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0x1B, [0x8001_0100, 0, 0, 0])), 3);
        m.mem.write(0x8001_0200, b"vam\0");
        assert_eq!(ret(bios::call(&mut m, 0xA0, 0x17, [0x8001_0100, 0x8001_0200, 0, 0])) as i32, -1);
        ret(bios::call(&mut m, 0xA0, 0x2B, [0x8001_0300, 0x5A, 3, 0]));
        assert_eq!(m.mem.bytes(0x8001_0300, 4), vec![0x5A, 0x5A, 0x5A, 0]);
        assert!(bios::call(&mut m, 0xA0, 0x00, [0; 4]).is_err());
    }
}
