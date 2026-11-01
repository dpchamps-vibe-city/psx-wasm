//! libsnd (chip spu, D59, D63): `_SsInit`, the one libsnd function whose
//! own code touches the SPU. libsnd is the games' music sequencer and
//! stays original guest code (D14); the rest of it reaches the SPU
//! through libspu's adapters ([`crate::libspu`]), and its state stays in
//! its own data, where the original keeps it.
//!
//! `_SsInit` (PsyQ 4.7 `ssinit.o`): the 16 registers from main volume to
//! ENDX (1F801D80h-9Eh) from its table in `.data`, `_SsVmInit(24)` (the
//! voice manager, original code), `_SsMarkCallback` cleared, then
//! `VBLANK_MINUS`, `_snd_openflag` and `_snd_ev_flag`. The table and the
//! globals are found through the relocated operands of its code (D61),
//! `_SsVmInit` through its `jal`.

use crate::{Adapter, M, Result};
use psx_bios::{Flow, GuestCall};
use psx_spu::mixer::{Mask, Volume};

pub const ADAPTERS: &[(&str, Adapter)] = &[("_SsInit", ss_init)];

/// `_SsInit()`. Returns 60 (`VBLANK_MINUS`, left in `v0`).
fn ss_init(m: &mut M, _a: [u32; 4]) -> Result {
    let text = m.sdk.at;
    let find = |m: &M, hi: u32, lo: u32, what: &str| {
        crate::libspu::pair(m, text + hi, text + lo).ok_or_else(|| format!("libsnd/ssinit at {text:#010x}: no {what} where PsyQ 4.7's object has it"))
    };
    let table = find(m, 0x14, 0x18, ".data")?;
    let vm_init = crate::libspu::jal(m, text + 0x38).ok_or_else(|| format!("libsnd/ssinit at {text:#010x}: no _SsVmInit call"))?;
    let marks = find(m, 0x44, 0x48, "_SsMarkCallback")?;
    let vblank_minus = find(m, 0x78, 0x7C, "VBLANK_MINUS")?;
    let openflag = find(m, 0x80, 0x84, "_snd_openflag")?;
    let ev_flag = find(m, 0x88, 0x8C, "_snd_ev_flag")?;
    // The table's 16 halfwords, register by register: a loop the
    // translation unrolls (no budget).
    let regs: [(Option<Volume>, Option<Mask>); 8] = [
        (Some(Volume::Main), None),
        (Some(Volume::Reverb), None),
        (None, Some(Mask::KeyOn)),
        (None, Some(Mask::KeyOff)),
        (None, Some(Mask::PitchMod)),
        (None, Some(Mask::Noise)),
        (None, Some(Mask::Reverb)),
        (None, Some(Mask::End)),
    ];
    for i in 0..16u32 {
        let v = m.mem.r16(table + 2 * i);
        let (vol, mask) = regs[(i / 2) as usize];
        m.with_spu(|s| match (vol, mask) {
            (Some(w), _) => s.set_common_volume(w, (i & 1) as usize, v),
            (_, Some(k)) => s.set_mask_half(k, i & 1 != 0, v),
            _ => {}
        });
    }
    m.release_irq();
    Ok(Flow::Call {
        call: GuestCall { addr: vm_init, args: [24, 0, 0, 0] },
        then: Box::new(move |m, _| {
            // 32 sequences' 16 mark callbacks: a pass per sequence (the
            // translation unrolls the inner loop).
            for i in 0..32 * 16 {
                m.mem.w32(marks + 4 * i, 0);
            }
            crate::libspu::spend(
                m,
                32,
                Box::new(move |m| {
                    m.mem.w32(vblank_minus, 60);
                    m.mem.w32(openflag, 0);
                    m.mem.w32(ev_flag, 0);
                    // No runtime call ends the original: no safepoint (D63).
                    m.quiet = true;
                    Flow::ret(60)
                }),
            )
        }),
    })
}
