//! libmcrd, libcard and libmcx (M2: no memory card, no PocketStation; the
//! card device arrives with M7). Commands are accepted and complete with
//! "card does not exist".

use crate::{Adapter, M, Result, ret};

pub const ADAPTERS: &[(&str, Adapter)] = &[
    ("MemCardInit", zero),
    ("MemCardEnd", zero),
    ("MemCardStart", zero),
    ("MemCardStop", zero),
    ("MemCardExist", start),
    ("MemCardAccept", start),
    ("MemCardOpen", not_exist),
    ("MemCardClose", zero),
    ("MemCardReadData", start),
    ("MemCardWriteData", start),
    ("MemCardReadFile", start),
    ("MemCardWriteFile", start),
    ("MemCardCreateFile", not_exist),
    ("MemCardDeleteFile", not_exist),
    ("MemCardFormat", not_exist),
    ("MemCardUnformat", not_exist),
    ("MemCardGetDirentry", not_exist),
    ("MemCardSync", sync),
    ("MemCardCallback", zero),
    ("InitCARD", zero),
    ("InitCARD2", zero),
    ("StartCARD", zero),
    ("StartCARD2", zero),
    ("StopCARD", zero),
    ("StopCARD2", zero),
    ("_bu_init", zero),
    ("McxStartCom", zero),
    ("McxStopCom", zero),
    ("McxSync", mcx_sync),
    ("McxCardType", mcx_start),
    ("McxAllInfo", mcx_start),
    ("McxExecApl", mcx_start),
];

/// MemCardSync results.
const DONE: u32 = 1;
const NOTHING: u32 = u32::MAX;
/// McErrCardNotExist.
const ERR_NO_CARD: u32 = 1;
/// McxErrNoCard.
const MCX_NO_CARD: u32 = 1;

#[derive(Debug, Default)]
pub struct Mc {
    /// The command started and not yet synced.
    pub pending: Option<u32>,
}

fn start(m: &mut M, _a: [u32; 4]) -> Result {
    m.sdk.mc.pending = Some(1);
    ret(1)
}

fn not_exist(_m: &mut M, _a: [u32; 4]) -> Result {
    ret(ERR_NO_CARD)
}

fn sync(m: &mut M, a: [u32; 4]) -> Result {
    let Some(c) = m.sdk.mc.pending.take() else { return ret(NOTHING) };
    if a[1] != 0 {
        m.mem.w32(a[1], c);
    }
    if a[2] != 0 {
        m.mem.w32(a[2], ERR_NO_CARD);
    }
    ret(DONE)
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

fn zero(_m: &mut M, _a: [u32; 4]) -> Result {
    ret(0)
}
