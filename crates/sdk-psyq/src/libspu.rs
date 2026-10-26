//! libspu (M2 stubs until the SPU device, M6): register-level calls are
//! accepted and dropped, keys report off, transfers complete at once. SPU
//! RAM allocation is real (libsnd depends on `SpuMalloc` results).

use crate::{Adapter, M, Result, ret};

pub const ADAPTERS: &[(&str, Adapter)] = &[
    ("SpuInit", spu_init),
    ("SpuStart", zero),
    ("SpuSetCommonAttr", zero),
    ("SpuSetVoiceAttr", zero),
    ("SpuSetKey", zero),
    ("SpuGetKeyStatus", zero),
    ("SpuGetAllKeysStatus", all_keys_status),
    ("SpuRGetAllKeysStatus", all_keys_status),
    ("SpuInitMalloc", init_malloc),
    ("SpuMalloc", malloc),
    ("SpuFree", free),
    ("SpuSetTransferMode", zero),
    ("SpuSetTransferStartAddr", identity),
    ("SpuWritePartly", second_arg),
    ("SpuWrite", second_arg),
    ("SpuIsTransferCompleted", one),
    ("SpuSetReverb", zero),
    ("SpuSetReverbDepth", zero),
    ("SpuSetReverbModeParam", zero),
    ("SpuSetReverbVoice", zero),
    ("SpuClearReverbWorkArea", zero),
    ("SpuSetNoiseClock", zero),
    ("SpuSetNoiseVoice", zero),
    ("SpuGetNoiseVoice", zero),
    ("SpuGetReverbVoice", zero),
    ("SpuGetVoiceEnvelope", spu_get_voice_envelope),
];

/// SPU RAM below this holds the capture buffers; reverb sits at the top.
const ALLOC_BASE: u32 = 0x1010;
const ALLOC_END: u32 = 0x8_0000;

#[derive(Debug, Default)]
pub struct Spu {
    /// Allocated blocks: start → size.
    pub blocks: std::collections::BTreeMap<u32, u32>,
    pub max_blocks: u32,
}

fn spu_init(m: &mut M, _a: [u32; 4]) -> Result {
    m.sdk.spu.blocks.clear();
    ret(0)
}

fn init_malloc(m: &mut M, a: [u32; 4]) -> Result {
    m.sdk.spu.blocks.clear();
    m.sdk.spu.max_blocks = a[0];
    ret(a[0])
}

/// First fit, 8-byte granules; -1 when full.
fn malloc(m: &mut M, a: [u32; 4]) -> Result {
    let size = a[0].div_ceil(8) * 8;
    if size == 0 || (m.sdk.spu.max_blocks != 0 && m.sdk.spu.blocks.len() as u32 >= m.sdk.spu.max_blocks) {
        return ret(u32::MAX);
    }
    let mut at = ALLOC_BASE;
    for (&s, &n) in &m.sdk.spu.blocks {
        if s >= at + size {
            break;
        }
        at = at.max(s + n);
    }
    if at + size > ALLOC_END {
        return ret(u32::MAX);
    }
    m.sdk.spu.blocks.insert(at, size);
    ret(at)
}

fn free(m: &mut M, a: [u32; 4]) -> Result {
    m.sdk.spu.blocks.remove(&a[0]);
    ret(0)
}

fn all_keys_status(m: &mut M, a: [u32; 4]) -> Result {
    m.mem.write(a[0], &[0; 24]);
    ret(0)
}

fn spu_get_voice_envelope(m: &mut M, a: [u32; 4]) -> Result {
    m.mem.w16(a[1], 0);
    ret(0)
}

fn identity(_m: &mut M, a: [u32; 4]) -> Result {
    ret(a[0])
}

fn second_arg(_m: &mut M, a: [u32; 4]) -> Result {
    ret(a[1])
}

fn one(_m: &mut M, _a: [u32; 4]) -> Result {
    ret(1)
}

fn zero(_m: &mut M, _a: [u32; 4]) -> Result {
    ret(0)
}
