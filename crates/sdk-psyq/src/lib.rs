//! PsyQ adapters (docs/design.md §5): thin Rust replacements for the SDK
//! functions that touch hardware registers or BIOS internals. Everything
//! else in the SDK runs as guest code.
//!
//! Each adapter takes o32 arguments and returns a [`Flow`]: a value, or a
//! wait with a continuation (the runtime's driver advances virtual time and
//! runs callbacks meanwhile). Semantics follow the SDK's own code, as
//! psyq-decomp records it.
//!
//! Scope: libetc's interrupt layer and libapi's `counter.o` (chip irq),
//! libgpu's `sys.o` (chip gpu), libpad's direct connection (chip sio0),
//! libapi's `pad.o`/`patch.o`/`first.o`, libcd's drive layer and the
//! streaming ring's interrupt (chip cd), and
//! libcard/libmcx's thin BIOS wrappers.
//!
//! A chip whose adapters don't cover a game runs its libraries as the
//! game's own code over register-level devices ([`DEVICE_LIBRARIES`],
//! D40, D59).

pub mod cases;
pub mod libapi;
pub mod libcd;
pub mod libetc;
pub mod libgpu;
pub mod libmc;
pub mod libpad;
pub mod layout;
pub mod validated;

use psx_bios::{Flow, Machine, Sdk};

pub type M = Machine<Psyq>;
pub type Result = std::result::Result<Flow<Psyq>, String>;
pub type Adapter = fn(&mut M, [u32; 4]) -> Result;

/// A register of a device the adapters share with the emulated chips (the
/// interrupt and DMA controllers, the root counters, GPUSTAT), read with
/// the device's own semantics; not guest I/O.
pub(crate) fn reg(m: &mut M, addr: u32, size: u32) -> std::result::Result<u32, String> {
    m.reg_read(addr, size)
}

/// Write such a register.
pub(crate) fn set_reg(m: &mut M, addr: u32, size: u32, v: u32) -> std::result::Result<(), String> {
    m.reg_write(addr, size, v)
}

/// Libraries that run as original code, their I/O-window accesses reaching
/// the runtime's register-level devices (`psx_bios::io`), instead of
/// being replaced.
/// - libpad (D40), when chip sio0 is emulated: its observable state
///   (PadGetState's sequence, buffer bytes, return values) is its own
///   state machine over a few SIO0 bytes per frame; the device is a port
///   of the oracle's.
/// - libgpu (D42), when chip gpu is emulated: the GPU front end takes
///   GP0/GP1 words, so libgpu's own code drives it through the ports and
///   DMA; an ordering table is one DMA linked-list walk in Rust.
/// - libetc (D45), when chip irq is emulated: its interrupt layer is
///   entered through the BIOS's custom exception exit (a longjmp into
///   `startIntr`'s `setjmp`) and leaves with ReturnFromException; both are
///   non-local exits the lift and link tiers lower to wasm exceptions.
/// - libspu and libsnd (D47): the SPU is register-level (`psx-spu`, exact
///   against the oracle), so libspu's own code programs it, and libsnd's
///   few direct register writes reach it too.
/// - libcd and libds (D58), when chip cd is emulated: the CD controller
///   (`psx_devices::cdc`) takes their commands and delivers sectors and XA
///   audio. Native, libcd's drive layer is adapters over the same
///   controller's core (D62).
/// - libpress: the MDEC (`psx_devices::mdec`) decodes what its own code
///   feeds it by DMA0, and DMA1 takes the pixels.
pub const DEVICE_LIBRARIES: &[&str] = &["libpad", "libgpu", "libetc", "libspu", "libsnd", "libcd", "libds", "libpress", "libapi"];

/// Adapter state.
#[derive(Debug, Default)]
pub struct Psyq {
    pub cd: libcd::Cd,
    pub mc: libmc::Mc,
    pub etc: libetc::Etc,
    pub pad: libpad::Pad,
    pub gpu: libgpu::Gpu,
    /// The guest address of the SDK function the running adapter replaces
    /// (D61): the runtime sets it before each call.
    pub at: u32,
}

impl Sdk for Psyq {
    fn on_event(m: &mut M, kind: u32, _data: u32) {
        m.log(&format!("sdk: unknown event {kind}"));
    }

    fn ready(_m: &M, _c: u32) -> bool {
        true
    }

    /// Vblank and DMA completion reach the guest as interrupts: through
    /// libetc's adapters' dispatcher ([`libetc`]) when chip irq is native,
    /// through libetc's own when it is emulated (D45). The pads latch their
    /// buttons ([`libpad`]).
    fn on_vblank(m: &mut M) {
        libpad::on_vblank(m);
    }

    fn irq_dispatch(m: &M) -> Option<u32> {
        libetc::dispatch_step(m)
    }

    fn irq_step(m: &mut M, step: u32) {
        if step == libpad::STEP {
            return libpad::irq_step(m, step);
        }
        libetc::irq_step(m, step)
    }

    fn chain_element(_m: &M, elem: u32) -> Option<u32> {
        (elem == libpad::ELEMENT).then_some(libpad::STEP)
    }

    fn stand_in(m: &mut M, addr: u32, state: u32) -> Option<psx_bios::StandIn> {
        libetc::stand_in(m, addr, state).or_else(|| libgpu::stand_in(m, addr, state)).or_else(|| libcd::stand_in(m, addr, state))
    }
}

pub(crate) fn ret(v: u32) -> Result {
    Ok(Flow::ret(v))
}

#[allow(dead_code)]
pub(crate) fn block(until: psx_bios::Wait, then: impl FnOnce(&mut M) -> Flow<Psyq> + 'static) -> Result {
    Ok(Flow::Block { until, then: Box::new(then) })
}

/// The hardware adapters (D59), by name, with their chip. The rest are
/// BIOS-internals adapters, which have no device to fall back to.
pub fn chip_adapters() -> Vec<(&'static str, &'static str)> {
    let mut v: Vec<(&'static str, &'static str)> = Vec::new();
    v.extend(libapi::COUNTERS.iter().map(|n| (*n, "irq")));
    v.extend(libetc::ADAPTERS.iter().map(|(n, _)| (*n, "irq")));
    v.extend(libpad::ADAPTERS.iter().map(|(n, _)| (*n, "sio0")));
    v.extend(libgpu::ADAPTERS.iter().map(|(n, _)| (*n, "gpu")));
    v.extend(libcd::ADAPTERS.iter().map(|(n, _)| (*n, "cd")));
    v
}

/// Every adapter, by SDK function name.
pub fn adapters() -> Vec<(&'static str, Adapter)> {
    let mut v: Vec<(&'static str, Adapter)> = Vec::new();
    v.extend(libapi::ADAPTERS);
    v.extend(libetc::ADAPTERS);
    v.extend(libpad::ADAPTERS);
    v.extend(libgpu::ADAPTERS);
    v.extend(libcd::ADAPTERS);
    v.extend(libmc::ADAPTERS);
    v
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use psx_bios::IrqStep;
    use psx_devices::mem::VecMem;
    use psxw_abi::BootOptions;

    pub struct Host {
        pub disc: Vec<[u8; 2352]>,
    }

    impl psx_bios::Host for Host {
        fn log(&mut self, _level: u32, _msg: &str) {}
        fn wait_vblank(&mut self, _frame: u64) {}
        fn disc_read(&mut self, lba: u32, out: &mut [u8; 2352]) -> bool {
            match self.disc.get(lba as usize) {
                Some(s) => {
                    *out = *s;
                    true
                }
                None => false,
            }
        }
        fn has_disc(&mut self) -> bool {
            !self.disc.is_empty()
        }
        fn pad_state(&mut self, port: u32) -> Vec<u8> {
            if port == 0 { vec![0xFF, 0xEF] } else { vec![] }
        }
        fn storage_read(&mut self, _name: &str) -> Option<Vec<u8>> {
            None
        }
        fn storage_write(&mut self, _name: &str, _data: &[u8]) -> bool {
            false
        }
        fn report(&mut self, _json: &str) {}
    }

    pub struct Regs(pub u32);

    impl psx_bios::Regs for Regs {
        fn sp(&self) -> u32 {
            self.0
        }
        fn set_sp(&mut self, v: u32) {
            self.0 = v;
        }
        fn gp(&self) -> u32 {
            0
        }
        fn set_gp(&mut self, _v: u32) {}
    }

    pub fn machine(disc: Vec<[u8; 2352]>, opts: BootOptions) -> M {
        Machine::new(Box::new(VecMem::default()), Box::new(Host { disc }), Box::new(Regs(0x801F_F000)), opts, Psyq::default())
    }

    /// A machine with the register-level devices on its bus, as the
    /// runtime has.
    pub fn machine_with_bus() -> M {
        let mut m = machine(vec![], BootOptions::default());
        m.io = Some(Default::default());
        m
    }

    /// Call an adapter and run its flow to completion.
    pub fn call(m: &mut M, f: Adapter, a: [u32; 4]) -> u32 {
        let r = f(m, a);
        finish(m, r)
    }

    std::thread_local! {
        static IRQ_CALLS: std::cell::RefCell<Vec<u32>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    /// Run the exception handler's queued steps as the runtime's driver
    /// does, with guest calls doing nothing: the adapters' dispatcher
    /// steps and stand-in functions run, and the guest functions they call
    /// are logged ([`irq_calls`]). Callbacks the kernel queues (`m.calls`)
    /// stay.
    pub fn drain(m: &mut M) {
        if m.crit || m.in_irq {
            return;
        }
        while let Some(s) = m.irq_calls.pop_front() {
            match s {
                IrqStep::Sdk(s) => Psyq::irq_step(m, s),
                IrqStep::Chain(c, _) => guest(m, c.addr),
                IrqStep::Exit => {}
            }
        }
    }

    /// A guest call: a stand-in runs (as the runtime's `untranslated`
    /// does), anything else is logged.
    pub fn guest(m: &mut M, addr: u32) {
        let mut state = 0;
        while let Some(step) = Psyq::stand_in(m, addr, state) {
            match step {
                psx_bios::StandIn::Call(c, next) => {
                    guest(m, c.addr);
                    state = next;
                }
                psx_bios::StandIn::Return(_) => return,
            }
        }
        IRQ_CALLS.with(|v| v.borrow_mut().push(addr));
    }

    /// The guest functions interrupts called since the last look.
    pub fn irq_calls() -> Vec<u32> {
        IRQ_CALLS.with(|v| std::mem::take(&mut *v.borrow_mut()))
    }

    /// Run a flow to completion the way the runtime's driver does (kernel
    /// callbacks stay queued; see [`drain`]).
    pub fn finish(m: &mut M, f: Result) -> u32 {
        let mut f = f.unwrap();
        loop {
            drain(m);
            match f {
                Flow::Return(v) => return v as u32,
                Flow::Block { until, then } => {
                    let mut guard = 0;
                    while !m.holds(until) {
                        let limit = m.clock.now + psx_devices::clock::CPU_HZ;
                        m.step(limit);
                        drain(m);
                        guard += 1;
                        assert!(guard < 100_000, "wait never satisfied: {until:?}");
                    }
                    f = then(m);
                }
                Flow::Call { call, then } => {
                    guest(m, call.addr);
                    f = then(m, 0);
                }
            }
        }
    }

    /// Let `n` vblanks pass, interrupts dispatched.
    pub fn vblanks(m: &mut M, n: u64) {
        let until = m.vblanks + n;
        let f: Result = Ok(Flow::Block { until: psx_bios::Wait::Vblanks(until), then: Box::new(|_| Flow::Return(0)) });
        finish(m, f);
    }
}
