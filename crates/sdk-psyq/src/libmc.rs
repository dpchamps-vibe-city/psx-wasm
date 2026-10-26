//! libcard's BIOS wrappers and libmcx (no PocketStation: a command to a
//! slot holding a memory card completes with McxErrInvalid, the result a
//! card that isn't a PocketStation gives; to an empty slot, with
//! McxErrNoCard). libmcrd runs as guest code over the BIOS card functions
//! (`psx-bios::card`).

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
/// McxErrNoCard, McxErrInvalid.
const MCX_NO_CARD: u32 = 1;
const MCX_INVALID: u32 = 2;

#[derive(Debug, Default)]
pub struct Mc {
    /// The command started and not yet synced, and its result.
    pub pending: Option<(u32, u32)>,
}

/// `McxCardType(port)`, `McxAllInfo(port, …)`, `McxExecApl(port, …)`:
/// ports 00h and 10h are slots 1 and 2.
fn mcx_start(m: &mut M, a: [u32; 4]) -> Result {
    m.cards_ready();
    let card = m.kernel.cards.slots[((a[0] >> 4) & 1) as usize].is_some();
    m.sdk.mc.pending = Some((1, if card { MCX_INVALID } else { MCX_NO_CARD }));
    ret(1)
}

fn mcx_sync(m: &mut M, a: [u32; 4]) -> Result {
    let Some((c, result)) = m.sdk.mc.pending.take() else { return ret(NOTHING) };
    if a[1] != 0 {
        m.mem.w32(a[1], c);
    }
    if a[2] != 0 {
        m.mem.w32(a[2], result);
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
