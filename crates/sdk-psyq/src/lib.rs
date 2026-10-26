//! PsyQ adapters (docs/design.md §5): thin Rust replacements for the SDK
//! functions that touch hardware registers or BIOS internals. Everything
//! else in the SDK runs as guest code.
//!
//! Each adapter takes o32 arguments and returns a [`Flow`]: a value, or a
//! wait with a continuation (the runtime's driver advances virtual time and
//! runs callbacks meanwhile). Semantics follow the SDK's own code, as
//! psyq-decomp records it.
//!
//! Scope: libapi's `counter.o`/`pad.o`/`patch.o`/`first.o`, the libcd data
//! path; libspu and libcard/libmcx as plausible stubs until their devices
//! exist (M6, M7).
//!
//! Some libraries need no adapters: they run as the game's own code over
//! register-level devices ([`DEVICE_LIBRARIES`], decision D40).

pub mod libapi;
pub mod libcd;
pub mod libmc;
pub mod layout;
pub mod libspu;
pub mod validated;

use psx_bios::{Flow, Machine, Sdk};

pub type M = Machine<Psyq>;
pub type Result = std::result::Result<Flow<Psyq>, String>;
pub type Adapter = fn(&mut M, [u32; 4]) -> Result;

/// Libraries that run as original code, their I/O-window accesses reaching
/// the runtime's register-level devices (`psx_bios::io`), instead of
/// being replaced.
/// - libpad (D40): its observable state (PadGetState's sequence, buffer
///   bytes, return values) is its own state machine over a few SIO0 bytes
///   per frame; the device is a port of the oracle's.
/// - libgpu (D42): the GPU front end takes GP0/GP1 words, so libgpu's own
///   code drives it through the ports and DMA; an ordering table is one
///   DMA linked-list walk in Rust.
/// - libetc (D45): its interrupt layer is entered through the BIOS's
///   custom exception exit (a longjmp into `startIntr`'s `setjmp`) and
///   leaves with ReturnFromException; both are non-local exits the lift
///   and link tiers lower to wasm exceptions.
pub const DEVICE_LIBRARIES: &[&str] = &["libpad", "libgpu", "libetc"];

/// Event kinds the adapters schedule.
pub mod ev {
    /// A CD command completes (data: command).
    pub const CD_COMPLETE: u32 = 1;
    /// The next sector of a read arrives (data: generation).
    pub const CD_SECTOR: u32 = 2;
}

/// Conditions blocked adapters wait for.
pub mod cond {
    pub const CD_COMMAND_DONE: u32 = 1;
    pub const CD_READ_DONE: u32 = 2;
    pub const CD_DATA_READY: u32 = 3;
}

/// Adapter state.
#[derive(Debug, Default)]
pub struct Psyq {
    pub cd: libcd::Cd,
    pub spu: libspu::Spu,
    pub mc: libmc::Mc,
}

impl Sdk for Psyq {
    fn on_event(m: &mut M, kind: u32, data: u32) {
        match kind {
            ev::CD_COMPLETE => libcd::on_complete(m, data),
            ev::CD_SECTOR => libcd::on_sector(m, data),
            _ => m.log(&format!("sdk: unknown event {kind}")),
        }
    }

    fn ready(m: &M, c: u32) -> bool {
        match c {
            cond::CD_COMMAND_DONE => m.sdk.cd.busy_until.is_none(),
            cond::CD_READ_DONE => m.sdk.cd.read.is_none(),
            cond::CD_DATA_READY => m.sdk.cd.ready_sector.is_some() || m.sdk.cd.read.is_none() && m.sdk.cd.streaming.is_none(),
            _ => true,
        }
    }

    /// Vblank and DMA completion reach the guest as interrupts (libetc
    /// runs over them, D45).
    fn on_vblank(_m: &mut M) {}
}

pub(crate) fn ret(v: u32) -> Result {
    Ok(Flow::ret(v))
}

pub(crate) fn block(until: psx_bios::Wait, then: impl FnOnce(&mut M) -> Flow<Psyq> + 'static) -> Result {
    Ok(Flow::Block { until, then: Box::new(then) })
}

/// Every adapter, by SDK function name.
pub fn adapters() -> Vec<(&'static str, Adapter)> {
    let mut v: Vec<(&'static str, Adapter)> = Vec::new();
    v.extend(libapi::ADAPTERS);
    v.extend(libcd::ADAPTERS);
    v.extend(libspu::ADAPTERS);
    v.extend(libmc::ADAPTERS);
    v
}

#[cfg(test)]
pub mod testing {
    use super::*;
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

    /// Call an adapter and run its flow to completion.
    pub fn call(m: &mut M, f: Adapter, a: [u32; 4]) -> u32 {
        let r = f(m, a);
        finish(m, r)
    }

    /// Run a flow to completion the way the runtime's driver does (no
    /// callbacks run; they stay queued).
    pub fn finish(m: &mut M, f: Result) -> u32 {
        let mut f = f.unwrap();
        loop {
            match f {
                Flow::Return(v) => return v as u32,
                Flow::Block { until, then } => {
                    let mut guard = 0;
                    while !m.holds(until) {
                        let limit = m.clock.now + psx_devices::clock::CPU_HZ;
                        m.step(limit);
                        guard += 1;
                        assert!(guard < 100_000, "wait never satisfied: {until:?}");
                    }
                    f = then(m);
                }
            }
        }
    }
}
