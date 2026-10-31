//! psx-bios as the oracle's `bios` device: a `BIOS_CALL` vector's call on
//! a machine with nothing else running, as the runner makes it on the
//! target's BIOS (its scratch set up, the call, v0 and the captures). A
//! call that calls the guest back (`qsort`, `lsearch`, `bsearch`) has no
//! guest here, so it is no vector.

use crate::commands::DeviceUnderTest;
use crate::store::Store;
use crate::threads::{MAIN_GP, MAIN_SP, NoSdk, NullHost, SimRegs};
use crate::vector::{BiosCall, Vector};
use anyhow::{Result, bail};
use psx_bios::{Flow, Machine};
use psx_devices::mem::VecMem;

pub struct Device;

impl DeviceUnderTest for Device {
    fn device(&self) -> &str {
        "bios"
    }

    fn name(&self) -> String {
        "psx-bios".into()
    }

    fn run(&mut self, v: &Vector, _: &Store) -> Result<Vec<u8>> {
        match v {
            Vector::BiosCall(b) => run(b),
            other => bail!("the bios device cannot run a {} vector", other.kind()),
        }
    }
}

/// The call's result in OP_BIOS_CALL's layout: v0, then each capture.
pub fn run(b: &BiosCall) -> Result<Vec<u8>> {
    let mut m = Machine::new(
        Box::new(VecMem::default()),
        Box::new(NullHost),
        Box::new(SimRegs { sp: MAIN_SP, gp: MAIN_GP }),
        psxw_abi::BootOptions::default(),
        NoSdk,
    );
    for (addr, bytes) in &b.setup {
        m.mem.write(*addr, bytes);
    }
    let v = match psx_bios::bios::call(&mut m, b.table, b.func, b.args).map_err(|e| anyhow::anyhow!(e))? {
        Flow::Return(v) => v as u32,
        Flow::Block { .. } | Flow::Call { .. } => bail!("{:02X}h:{:02X}h blocked", b.table, b.func),
    };
    let mut out = v.to_le_bytes().to_vec();
    for &(addr, len) in &b.captures {
        out.extend(m.mem.bytes(addr, len as usize));
    }
    Ok(out)
}
