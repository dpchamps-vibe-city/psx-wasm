//! Game-agnostic SDK services behind the `replace` tier. Host-agnostic: the
//! same crate is built into `runtime.wasm` for the browser and linked
//! natively into the wasmtime host for tests. All game-visible time is
//! virtual and deterministic; wall-clock time only paces presentation.

pub use psx_gte as gte;

pub mod kernel {
    //! BIOS A0/B0/C0 surface used by libapi: events, callbacks, critical
    //! sections, `InitHeap`, file API over the CD, `FlushCache`, setjmp
    //! support. Boot: load the PS-X EXE from disc into RAM, zero BSS, set
    //! `__guest_sp`, call `guest_main`.
}

pub mod clock {
    //! Virtual timeline: vblank (59.94/50 Hz), root counters, scheduled
    //! device completions, the safepoint quantum. Events are delivered in a
    //! fixed order at safepoints and blocking calls.
}

pub mod gpu {
    //! 1024x512x16bpp VRAM model and software rasterizer for every
    //! primitive type; DrawOTag walks the ordering table in guest memory.
    //! Load/Store/Move/ClearImage, draw and display environments, and the
    //! display area converted to an RGBA frame for the host.
}

pub mod spu {
    //! 512 KiB SPU RAM, 24 ADPCM voices with ADSR/pitch/noise, reverb, CD
    //! input. Renders one vblank of samples per frame.
}

pub mod cd {
    //! Disc image (.bin/.cue, .iso), ISO9660 lookup, sector reads scheduled
    //! on the clock (realistic or instant), XA-ADPCM streaming to the SPU.
}

pub mod pad {
    //! Pad buffers as PadInitDirect/PadGetState expect, filled per vblank.
}

pub mod memcard {
    //! 128 KiB card images behind libmcrd/libcard/libmcx.
}

/// What the runtime needs from its host (browser worker or native).
pub trait Host {
    /// Block until wall-clock time catches up with the virtual vblank.
    fn wait_for_vblank(&mut self);
    /// Hand a finished frame (RGBA8, width x height) to presentation.
    fn present(&mut self, rgba: &[u8], width: u32, height: u32);
    /// Queue one frame of interleaved stereo samples.
    fn push_audio(&mut self, samples: &[i16]);
    /// Current digital/analog pad state for a port.
    fn pad_state(&mut self, port: u8) -> u16;
    /// Synchronous read of disc sectors (OPFS sync handle or a file).
    fn read_disc(&mut self, lba: u32, buf: &mut [u8]);
}
