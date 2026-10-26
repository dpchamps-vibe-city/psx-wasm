//! libcard's BIOS wrappers and libmcx (no PocketStation: commands
//! complete with "no card"). libmcrd runs as guest code over the BIOS card
//! functions (`psx-bios::card`).

use crate::{Adapter, M, Result, ret};

pub const ADAPTERS: &[(&str, Adapter)] = &[
    ("InitCARD", cards),
    ("InitCARD2", cards),
    ("StartCARD", cards),
    ("StartCARD2", cards),
    ("StopCARD", zero),
    ("StopCARD2", zero),
    ("_bu_init", cards),
    ("McxStartCom", zero),
    ("McxStopCom", zero),
    ("McxSync", mcx_sync),
    ("McxCardType", mcx_start),
    ("McxAllInfo", mcx_start),
    ("McxExecApl", mcx_start),
];

/// McxSync results.
const DONE: u32 = 1;
const NOTHING: u32 = u32::MAX;
/// McxErrNoCard.
const MCX_NO_CARD: u32 = 1;

#[derive(Debug, Default)]
pub struct Mc {
    /// The command started and not yet synced.
    pub pending: Option<u32>,
}

fn mcx_start(m: &mut M, _a: [u32; 4]) -> Result {
    m.sdk.mc.pending = Some(1);
    ret(1)
}

fn mcx_sync(m: &mut M, a: [u32; 4]) -> Result {
    let Some(c) = m.sdk.mc.pending.take() else { return ret(NOTHING) };
    if a[1] != 0 {
        m.mem.w32(a[1], c);
    }
    if a[2] != 0 {
        m.mem.w32(a[2], MCX_NO_CARD);
    }
    ret(DONE)
}

/// The BIOS wrappers (`InitCARD` → B(4Ah), …): the cards are ready
/// (`psx-bios::card`), whatever `_patch_card` would patch in a Sony BIOS.
fn cards(m: &mut M, _a: [u32; 4]) -> Result {
    m.cards_ready();
    ret(0)
}

fn zero(_m: &mut M, _a: [u32; 4]) -> Result {
    ret(0)
}
