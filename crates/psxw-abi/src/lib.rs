//! The contract between the guest (per game, built by `psxw-link`), the
//! runtime (game-agnostic, `psx-runtime`) and the host (wasmtime or a
//! browser worker). All three depend on this crate; nothing here may depend
//! on a particular game. See `docs/design.md` §1, §2, §7.
//!
//! `game.wasm` is the guest module and the runtime module merged: the
//! guest's `psx.*` imports bind to the runtime's exports, the runtime's
//! `guest.*` imports to the guest's exports. What remains imported is the
//! host interface ([`host`]); what is exported is [`exports`].

#![no_std]

pub mod render;
pub mod replay;

/// Bumped whenever the host interface, the guest/runtime seam or the boot
/// options change. `game.wasm` exports it; hosts refuse a mismatch.
///
/// - 1: M2's host interface ([`host`]).
/// - 2: adds the renderer command interface ([`render`], version
///   [`render::RENDER_VERSION`]) and its host imports
///   [`render::imports`] (`render_submit`, `render_readback`, `present`).
/// - 3: device libraries' I/O (`psx.io_access`, `psx.io_trap`, D40) and the
///   disc-free entry points (`psxw_conform_init`, `psxw_call`,
///   `psxw_run_vblanks`).
/// - 4: non-local exits for the BIOS's interrupt exit (D45):
///   `guest.guest_invoke_irq`, `psx.return_from_exception`, and wasm
///   exception handling in the module.
/// - 5: audio output, `host.audio` (M6).
/// - 6: recording hooks: GPU draws ([`render::imports`]
///   `gpu_record_begin`, `gpu_record`, M4) when
///   [`BootOptions::TRACE_RECORD_GPU`] is set, and SPU windows
///   (`host.spu_record_begin`, `host.spu_record`, M6) when
///   [`BootOptions::TRACE_RECORD_SPU`] is.
/// - 7: mods (M11): the mod area ([`phys::MOD_DATA`]), the guest's
///   `guest.psxmod_init` and `guest.psxmod_loaded` (data patches), and
///   `psx.mod_log`.
/// - 8: one-shot scripting: `psxw_execute` queues a guest call; its result
///   is an `execute` event through `host.report`.
/// - 9: the guest's window is where the guest says (`guest.guest_window`):
///   [`WINDOW_BASE`], or [`DEBUG_WINDOW_BASE`] in a debug build. The
///   runtime and hosts read it; host pointers may lie above 2 GiB.
/// - 10: the host's vblank in two (D54): `host.vblank`, which never
///   suspends and carries everything the guest can see, and
///   `host.wait_vblank`, which only paces; `psxw_js_enter` and
///   `psxw_js_leave` bracket calls into JS, and the runtime holds
///   `wait_vblank` calls while JS is on the stack.
/// - 11: guest threads (D57): the guest exports binaryen Asyncify's
///   control functions ([`guest::ASYNCIFY_START_UNWIND`] and the rest),
///   instrumented when the game switches threads and stubs otherwise; the
///   BIOS keeps the kernel's thread tables in guest RAM ([`kernel`]).
/// - 12: an adapter call carries the replaced function's guest address
///   ([`psx::ADAPTER`], D61).
/// - 13: overlay residency (D69): `guest.psxmod_loaded` hears of every
///   disc write into RAM, mods or not, and a call into an overlay region
///   with nothing resident to run traps through [`psx::NOT_RESIDENT`].
/// - 14: the console's memory map (D70, [`map`]): the guest maps every
///   access but RAM's through `psx.map_access` and every function pointer
///   but RAM's through `psx.map_code`; psx-wasm's own data and synthetic
///   code move to [`OWN_BASE`]; hosts reach guest bytes through
///   [`exports::GUEST_PTR`].
/// - 15: a function pointer to a BIOS stand-in ([`BIOS_STAND_INS`]) reaches
///   its thunk in [`BIOS_THUNKS`], which calls `psx.bios` with the
///   stand-in's address.
/// - 16: the safepoint budget counts the original's instructions in fixed
///   point ([`guest::BUDGET_UNIT`], D71): guest code spends it per block
///   and checks it at loop headers; `guest_budget_get`/`set` are in those
///   units and may be at or below 0 between checks.
pub const ABI_VERSION: u32 = 16;

/// The audio output rate: the SPU's, one sample per 768 CPU cycles.
pub const AUDIO_HZ: u32 = 44_100;

/// wasm address of window offset 0 (guest physical address 0). Folded into
/// every guest load/store's `memarg` offset; the offset itself comes from
/// [`map`].
pub const WINDOW_BASE: u32 = 0x0100_0000;

/// A debug build's window (D53): every KSEG0 address (`0x80xx_xxxx`) is its
/// own wasm address, so a debugger reading DWARF finds globals, the stack
/// and what pointers point at where the game put them. The memory spans
/// 2 GiB below it, which nothing touches.
pub const DEBUG_WINDOW_BASE: u32 = 0x8000_0000;

/// Initial memory pages for a window at `base`: the window's 16 MiB end it.
pub const fn window_pages(base: u32) -> u64 {
    (base as u64 + WINDOW_MASK as u64 + 1).div_ceil(65536)
}

/// The window is 16 MiB: every window offset is below it. The differential
/// harness (psxw-difftest) masks guest addresses with it instead of
/// mapping them; psx-wasm's own addresses ([`OWN_BASE`]) are their window
/// offset under it.
pub const WINDOW_MASK: u32 = 0x00FF_FFFF;

/// psx-wasm's own guest addresses: AUX data, mods' data and stacks, and
/// synthetic code. KSEG0 addresses in the physical space the console leaves
/// unmapped (from 16 MiB up), so no original pointer reaches them; their
/// low 24 bits are their window offset ([`own`]).
pub const OWN_BASE: u32 = 0x8100_0000;

/// The own guest address of window offset `offset`.
pub const fn own(offset: u32) -> u32 {
    OWN_BASE | offset
}

/// The runtime is linked with `--global-base` here; everything Rust owns
/// lives above the default guest window (below a debug build's, its heap
/// above it).
pub const RUNTIME_BASE: u32 = 0x0200_0000;

/// The kernel's tables in guest RAM (D57, M13, D68): what the BIOS lays
/// out for games to read and edit directly (psx-spx "BIOS Memory Map").
/// The rest of the kernel's state stays in the runtime. Addresses are
/// physical; the BIOS points at them through KSEG1, as the console's does.
pub mod kernel {
    use core::ops::Range;
    /// The garbage area at 0: a copy of the exception vector, its first
    /// word smashed to 3 (psx-spx).
    pub const GARBAGE: u32 = 0x00;
    /// RAM size in megabytes, then 0 and FFh (psx-spx).
    pub const RAM_SIZE: u32 = 0x60;
    /// The exception vector (four opcodes, KSEG0).
    pub const EXCEPTION_VECTOR: u32 = 0x80;
    /// The A(nnh), B(nnh) and C(nnh) function vectors (four opcodes each).
    pub const A0_VECTOR: u32 = 0xA0;
    pub const B0_VECTOR: u32 = 0xB0;
    pub const C0_VECTOR: u32 = 0xC0;
    /// The vectors' dispatchers, which read the tables (OpenBIOS's code;
    /// psx-spx's kernel code area): eight opcodes each, A0h's first.
    pub const DISPATCHERS: u32 = 0x5C0;
    /// The SYSTEM.CNF `BOOT` line's argument (80h bytes).
    pub const BOOT_ARGUMENT: u32 = 0x180;
    /// The call tables: an entry per function (psx-spx). C(80h..) reads
    /// B0h's table, which follows C0h's.
    pub const A0_TABLE: u32 = 0x200;
    pub const A0_LEN: u32 = 0xC0;
    pub const C0_TABLE: u32 = 0x674;
    pub const C0_LEN: u32 = 0x80;
    pub const B0_TABLE: u32 = 0x874;
    pub const B0_LEN: u32 = 0x100;
    /// The table of tables: a (pointer, size) pair per table from 0x100.
    /// These are filled: the exception control blocks, the process control
    /// block, the thread control blocks, the event control blocks, the
    /// file control blocks and the device control blocks.
    pub const EXCB_ENTRY: u32 = 0x100;
    pub const PCB_ENTRY: u32 = 0x108;
    pub const TCB_ENTRY: u32 = 0x110;
    /// The file control blocks (psx-spx: 16 of 2Ch bytes, `fd` 0-15) and
    /// the device control blocks (10 of 50h bytes), at fixed addresses in
    /// the kernel's data, after `strtok`'s buffer (psx-bios).
    pub const FCB_ENTRY: u32 = 0x140;
    pub const DCB_ENTRY: u32 = 0x150;
    pub const FCBS: u32 = 0x7300;
    pub const FCB_SIZE: u32 = 0x2C;
    pub const FILES: u32 = 16;
    pub const DCBS: u32 = FCBS + FILES * FCB_SIZE;
    pub const DCB_SIZE: u32 = 0x50;
    pub const DEVICES: u32 = 10;
    /// The kernel's data around them (psx-bios: the pad buffers, its
    /// handler elements, `strtok`'s buffer, the control blocks, the device
    /// names, the default exception exit and the CD driver's sector
    /// buffer).
    pub const DATA: Range<u32> = 0x7000..0x8800;
    /// The PCB: one word, the current thread's TCB.
    pub const PCB: u32 = 0xE000;
    /// The TCBs (SYSTEM.CNF `TCB=`, at most [`MAX_TCBS`]); TCB 0 is the
    /// main thread.
    pub const TCBS: u32 = 0xE010;
    pub const TCB_SIZE: u32 = 0xC0;
    pub const MAX_TCBS: u32 = 32;
    /// TCB fields: status, then r0..r31 from `REGS`, then cop0 epc, hi,
    /// lo, sr, cause.
    pub const TCB_STATUS: u32 = 0x00;
    pub const TCB_REGS: u32 = 0x08;
    pub const TCB_EPC: u32 = 0x88;
    pub const TCB_SR: u32 = 0x94;
    pub const TCB_FREE: u32 = 0x1000;
    pub const TCB_USED: u32 = 0x4000;
    /// The exception control blocks: per interrupt priority 0..3, the head
    /// of its chain of elements (each linked through its first word) and an
    /// unused word. Just below the PCB.
    pub const EXCB: u32 = 0xDFE0;
    pub const EXCB_SIZE: u32 = 0x20;
    /// The event control blocks (D68; SYSTEM.CNF `EVENT=`, at most
    /// [`MAX_EVENTS`]), after the TCBs: per event its class, status, spec,
    /// mode and handler (psx-spx "BIOS Control Blocks").
    pub const EVCB_ENTRY: u32 = 0x120;
    pub const EVCBS: u32 = TCBS + MAX_TCBS * TCB_SIZE;
    pub const EVCB_SIZE: u32 = 0x1C;
    pub const MAX_EVENTS: u32 = 64;
    /// Kernel RAM a game may read and write at constant addresses: the
    /// lift's kernel rule accepts these and still fails on the rest (the
    /// patch area at DF80h among it). The vectors, variables and table of
    /// tables; the boot argument, the call tables and their dispatchers;
    /// the kernel's data; the control blocks.
    pub const RAM_BACKED: [Range<u32>; 4] =
        [0..DCB_ENTRY + 8, BOOT_ARGUMENT..B0_TABLE + 4 * B0_LEN, DATA, EXCB..EVCBS + MAX_EVENTS * EVCB_SIZE];

    /// Whether physical address `addr` lies in [`RAM_BACKED`].
    pub fn ram_backed(addr: u32) -> bool {
        RAM_BACKED.iter().any(|r| r.contains(&(addr & 0x1FFF_FFFF)))
    }
}

/// The window's layout: what lies at each window offset ([`map`] says
/// which guest addresses reach it).
pub mod phys {
    pub const RAM: core::ops::Range<u32> = 0x0000_0000..0x0020_0000;
    /// clang-only data (string literals, constant pools) with no original
    /// address, at its own guest addresses ([`super::own`]).
    pub const AUX_DATA: core::ops::Range<u32> = 0x0020_0000..0x0060_0000;
    /// Mods' globals, stacks and heaps (M11), at their own guest addresses;
    /// never the game's RAM. A static build's mods share it, its stack at
    /// the top ([`MOD_STACK`]); dynamically loaded mods each get a slice.
    pub const MOD_DATA: core::ops::Range<u32> = 0x0060_0000..0x0080_0000;
    /// The stack static mods run on, at the top of [`MOD_DATA`].
    pub const MOD_STACK: core::ops::Range<u32> = 0x007F_0000..0x0080_0000;
    pub const SCRATCHPAD: core::ops::Range<u32> = 0x0080_0000..0x0080_0400;
    /// The I/O ports. Nothing backs them; debug builds trap here.
    pub const IO: core::ops::Range<u32> = 0x0080_1000..0x0080_2000;
    /// The BIOS ROM (zeros: the BIOS is the runtime's).
    pub const BIOS_ROM: core::ops::Range<u32> = 0x00C0_0000..0x00C8_0000;
    /// Runtime-private: the GTE register file.
    pub const RUNTIME_PRIVATE: core::ops::Range<u32> = 0x00F0_0000..0x00F0_0100;
    /// Where an access to an expansion region goes: a load finds the
    /// region's open-bus byte here, a store's bytes are dropped here.
    pub const OPEN_BUS: core::ops::Range<u32> = 0x00F0_0100..0x00F0_0110;
    /// KSEG2's cache control registers.
    pub const CACHE_CONTROL: core::ops::Range<u32> = 0x00FE_0000..0x00FE_0200;
}

/// Synthetic MIPS addresses for functions that have no original address
/// (mod functions whose address is taken or stored in game data).
pub const SYNTHETIC_CODE: core::ops::Range<u32> = own(0x0020_0000)..own(0x0040_0000);

/// The BIOS's stand-ins (D68): a BIOS ROM address per A0h, B0h and C0h
/// function, which kernel RAM's tables hold (psx-bios `tables`). A game
/// that saves an entry and calls it through a pointer calls one.
pub const BIOS_STAND_INS: core::ops::Range<u32> = 0xBFC0_A000..0xBFC0_A900;

/// The stand-ins' dispatch-table slots, at the top of [`SYNTHETIC_CODE`]:
/// a thunk each, which calls `psx.bios` with its stand-in's address.
pub const BIOS_THUNKS: core::ops::Range<u32> =
    SYNTHETIC_CODE.end - (BIOS_STAND_INS.end - BIOS_STAND_INS.start)..SYNTHETIC_CODE.end;

/// Dispatch table index for a function's address: RAM's first copy or
/// [`SYNTHETIC_CODE`] (any guest function pointer: [`map::code_index`]).
/// Unmapped entries trap with the address.
#[inline]
pub const fn table_index(fp: u32) -> u32 {
    (fp & 0x003F_FFFC) >> 2
}
pub const TABLE_LEN: u32 = 0x0010_0000;

/// The console's memory map (psx-spx "Memory Map" and "Memory Control",
/// D70): what a guest address reaches, as window bytes, an expansion
/// region's open bus, or a bus error.
///
/// - **Main RAM**, 2 MiB mirrored four times in the first 8 MiB, through
///   KUSEG, KSEG0 and KSEG1.
/// - **The scratchpad**, 1 KiB at `1F80_0000`, through KUSEG and KSEG0
///   (not KSEG1).
/// - **The I/O ports**, 4 KiB at `1F80_1000`.
/// - **The expansion regions** at the sizes the BIOS leaves them
///   (DEV0 512 KiB at `1F00_0000`, DEV8 128 bytes at `1F80_2000`, DEV1 one
///   byte at `1FA0_0000`), with nothing attached: loads read FFh, 00h and
///   00h (Beetle's; psx-spx doesn't say), stores do nothing.
/// - **The BIOS ROM**, 512 KiB at `1FC0_0000` (not mirrored): stores do
///   nothing.
/// - **KSEG2's cache control**, 512 bytes at `FFFE_0000`.
/// - psx-wasm's own data at [`OWN_BASE`] (KSEG0).
///
/// Anything else is a bus error, KUSEG above 512 MiB included.
///
/// Guest code takes RAM through KUSEG and KSEG0 inline ([`FAST_MASK`]) and
/// calls `psx.map_access` for anything else.
pub mod map {
    use super::phys;
    use core::ops::Range;

    /// An address whose bits here are clear is RAM through KUSEG or KSEG0,
    /// at window offset `addr & RAM_MIRROR`: guest code's inline path.
    pub const FAST_MASK: u32 = 0x7F80_0000;
    /// RAM's mirrors fold onto its 2 MiB.
    pub const RAM_MIRROR: u32 = 0x001F_FFFF;
    /// Guest addresses of psx-wasm's own data, through KSEG0 only.
    pub const OWN: Range<u32> = super::own(phys::AUX_DATA.start)..super::own(phys::MOD_DATA.end);
    /// The I/O ports' physical addresses.
    pub const IO: Range<u32> = 0x1F80_1000..0x1F80_2000;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Region {
        Ram,
        Scratchpad,
        Io,
        /// An expansion region with nothing attached: loads read this byte,
        /// stores do nothing.
        Expansion(u8),
        /// Read-only: stores do nothing.
        BiosRom,
        CacheControl,
        /// psx-wasm's own data ([`OWN`]).
        Own,
    }

    /// Where a guest address's byte is.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Place {
        pub region: Region,
        /// Its window offset ([`phys::OPEN_BUS`] in an expansion region).
        pub offset: u32,
        /// Bytes from it to the end of its region (or RAM's mirror).
        pub len: u32,
    }

    impl Place {
        /// The window offset a store of `len` bytes from here writes:
        /// [`phys::OPEN_BUS`] where stores do nothing.
        pub fn store_offset(self) -> u32 {
            match self.region {
                Region::Expansion(_) | Region::BiosRom => phys::OPEN_BUS.start,
                _ => self.offset,
            }
        }
    }

    /// What guest address `addr` reaches; `None`: a bus error.
    pub fn place(addr: u32) -> Option<Place> {
        let at = |region, base: u32, size: u32, window: u32| {
            let o = addr.wrapping_sub(base) & (size - 1);
            Some(Place { region, offset: window + o, len: size - o })
        };
        let p = addr & 0x1FFF_FFFF;
        match addr >> 29 {
            // KSEG2: the cache control registers only.
            6 | 7 => {
                let o = addr.wrapping_sub(0xFFFE_0000);
                let size = phys::CACHE_CONTROL.end - phys::CACHE_CONTROL.start;
                return if o < size { at(Region::CacheControl, 0xFFFE_0000, size, phys::CACHE_CONTROL.start) } else { None };
            }
            // KUSEG above 512 MiB.
            1..=3 => return None,
            4 if OWN.contains(&addr) => return Some(Place { region: Region::Own, offset: addr & super::WINDOW_MASK, len: OWN.end - addr }),
            _ => {}
        }
        let open = |byte, base: u32, size: u32| {
            let o = p - base;
            Some(Place { region: Region::Expansion(byte), offset: phys::OPEN_BUS.start, len: size - o })
        };
        match p {
            0..0x0080_0000 => at(Region::Ram, 0, RAM_MIRROR + 1, 0),
            0x1F00_0000..0x1F08_0000 => open(0xFF, 0x1F00_0000, 0x8_0000),
            0x1F80_0000..0x1F80_0400 if addr >> 29 != 5 => at(Region::Scratchpad, 0x1F80_0000, 0x400, phys::SCRATCHPAD.start),
            0x1F80_1000..0x1F80_2000 => at(Region::Io, 0x1F80_1000, 0x1000, phys::IO.start),
            0x1F80_2000..0x1F80_2080 => open(0, 0x1F80_2000, 0x80),
            0x1FA0_0000..0x1FA0_0001 => open(0, 0x1FA0_0000, 1),
            0x1FC0_0000..0x1FC8_0000 => at(Region::BiosRom, 0x1FC0_0000, 0x8_0000, phys::BIOS_ROM.start),
            _ => None,
        }
    }

    /// The window offset of a load (or a store) of `len` bytes at `addr`,
    /// all in one region; `None`: a bus error. An expansion region's load
    /// finds its byte at [`phys::OPEN_BUS`] only once the caller puts it
    /// there. No bytes (a bulk copy of 0) reach nothing, wherever `addr` is.
    pub fn access(addr: u32, len: u32, store: bool) -> Option<u32> {
        if len == 0 {
            return Some(phys::OPEN_BUS.start);
        }
        let p = place(addr)?;
        let room = match p.region {
            Region::Expansion(_) => p.len.min(phys::OPEN_BUS.end - phys::OPEN_BUS.start),
            Region::BiosRom if store => phys::OPEN_BUS.end - phys::OPEN_BUS.start,
            _ => p.len,
        };
        (len <= room).then(|| if store { p.store_offset() } else { p.offset })
    }

    /// The dispatch-table index of function pointer `fp`: code in RAM
    /// (through any segment and mirror) or [`super::SYNTHETIC_CODE`].
    /// `None`: no code can be there (a bus error, or a region psx-wasm
    /// has no code for: the scratchpad, I/O, the BIOS ROM, expansion).
    pub fn code_index(fp: u32) -> Option<u32> {
        if super::SYNTHETIC_CODE.contains(&fp) {
            return Some(super::table_index(fp));
        }
        if super::BIOS_STAND_INS.contains(&fp) && fp & 3 == 0 {
            return Some(super::table_index(super::BIOS_THUNKS.start + (fp - super::BIOS_STAND_INS.start)));
        }
        match place(fp)? {
            Place { region: Region::Ram, offset, .. } => Some(offset >> 2),
            _ => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn regions() {
            let off = |a| place(a).map(|p| (p.region, p.offset));
            // RAM's four mirrors through KUSEG, KSEG0 and KSEG1.
            for a in [0x0000_1234, 0x8020_1234, 0xA040_1234, 0x8060_1234] {
                assert_eq!(off(a), Some((Region::Ram, 0x1234)));
            }
            assert_eq!(off(0x8080_0000), None);
            // The scratchpad isn't in KSEG1 and is 1 KiB.
            assert_eq!(off(0x9F80_03FC), Some((Region::Scratchpad, 0x80_03FC)));
            assert_eq!(off(0x1F80_0000), Some((Region::Scratchpad, 0x80_0000)));
            assert_eq!(off(0xBF80_0000), None);
            assert_eq!(off(0x1F80_0400), None);
            assert_eq!(off(0xBF80_1070), Some((Region::Io, 0x80_1070)));
            assert_eq!(off(0xBFC0_0100), Some((Region::BiosRom, 0xC0_0100)));
            assert_eq!(off(0xBFC8_0000), None);
            assert_eq!(place(0x1F00_0084).map(|p| p.region), Some(Region::Expansion(0xFF)));
            assert_eq!(place(0x1F08_0000), None);
            assert_eq!(off(0xFFFE_0130), Some((Region::CacheControl, 0xFE_0130)));
            assert_eq!(off(0xFFFE_0200), None);
            assert_eq!(off(0xC000_0000), None);
            // KUSEG above 512 MiB is no mirror; own data is KSEG0 only.
            assert_eq!(off(0x2000_0000), None);
            assert_eq!(off(0x8120_0010), Some((Region::Own, 0x20_0010)));
            assert_eq!(off(0x0120_0010), None);
            // Every RAM address the inline path takes, the map agrees with.
            for a in [0, 0x007F_FFFF, 0x8000_0000, 0x807F_FFFF] {
                assert_eq!(a & FAST_MASK, 0);
                assert_eq!(off(a), Some((Region::Ram, a & RAM_MIRROR)));
            }
        }

        #[test]
        fn accesses() {
            assert_eq!(access(0x1F80_03FC, 4, false), Some(0x80_03FC));
            assert_eq!(access(0x1F80_03FE, 4, false), None);
            assert_eq!(access(0xBFC0_0000, 4, true), Some(phys::OPEN_BUS.start));
            assert_eq!(access(0x1F00_0000, 4, false), Some(phys::OPEN_BUS.start));
            assert_eq!(access(0x1F00_0000, 0x100, false), None);
            assert!(access(0x0080_0000, 0, true).is_some());
            assert_eq!(code_index(0xA001_0000), Some(0x4000));
            assert_eq!(code_index(0x8021_0000), Some(0x4000));
            assert_eq!(code_index(super::super::SYNTHETIC_CODE.start), Some(0x8_0000));
            assert_eq!(code_index(0x1F80_0000), None);
            assert_eq!(code_index(0x0080_0000), None);
            // A BIOS stand-in reaches its thunk; the rest of ROM has no code.
            assert_eq!(code_index(0xBFC0_A004), Some(super::super::table_index(super::super::BIOS_THUNKS.start + 4)));
            assert_eq!(code_index(0xBFC0_0000), None);
        }
    }
}

/// Every dispatch-table entry has this wasm type, the "o32 indirect ABI":
/// `(a0, a1, a2, a3: i32) -> i64` where the result is `v0 | (v1 << 32)`.
/// Arguments beyond the fourth live on the guest stack at `sp + 16`, exactly
/// as in o32. Adapters to each function's real signature are generated by
/// the linker from prototypes.
pub mod indirect_abi {
    pub const PARAMS: usize = 4;
    pub const STACK_ARG_OFFSET: u32 = 16;
}

/// The guest's view of the runtime: module `psx`.
pub mod psx {
    pub const MODULE: &str = "psx";
    /// `() -> ()`: the loop-back-edge budget ran out; advance virtual time
    /// by one quantum and deliver due events.
    pub const SAFEPOINT: &str = "safepoint";
    /// `(addr) -> i64`: control reached a guest address with no
    /// implementation; traps with the address.
    pub const UNTRANSLATED: &str = "untranslated";
    /// `(table, fn, a0, a1, a2, a3) -> i64`: BIOS vector call (A0h, B0h,
    /// C0h: through the table in kernel RAM), or, with `table` a stand-in's
    /// address ([`super::BIOS_STAND_INS`]), that stand-in's function: a
    /// call through a pointer to it.
    pub const BIOS: &str = "bios";
    /// `(a0, a1, a2, a3) -> i32`: `syscall`.
    pub const SYSCALL: &str = "syscall";
    /// `(code) -> ()`: `break`.
    pub const BREAK: &str = "brk";
    /// `(pc) -> !`: `add`/`sub` overflow exception.
    pub const OVERFLOW: &str = "overflow";
    /// `(pc, target) -> !`: a jump table read a target outside the table.
    pub const BAD_JUMP: &str = "bad_jump";
    /// `(addr, a0, a1, a2, a3) -> i64`: a call into BIOS kernel RAM.
    pub const KERNEL_CALL: &str = "kernel_call";
    pub const COP0_READ: &str = "cop0_read";
    pub const COP0_WRITE: &str = "cop0_write";
    pub const RFE: &str = "rfe";
    pub const GTE_MFC2: &str = "gte_mfc2";
    pub const GTE_CFC2: &str = "gte_cfc2";
    pub const GTE_MTC2: &str = "gte_mtc2";
    pub const GTE_CTC2: &str = "gte_ctc2";
    pub const GTE_EXEC: &str = "gte_exec";
    /// `(window offset, kind) -> ()`: an access to the I/O window by a
    /// function of a device library (D40): a load calls it before reading,
    /// a store after writing; `kind` is the size plus 0x100 for stores. The
    /// runtime's devices supply or take the value through the window.
    pub const IO_ACCESS: &str = "io_access";
    /// `(window offset, kind) -> ()`: the same, in any other function of a
    /// debug build: a trap (the function needs an adapter).
    pub const IO_TRAP: &str = "io_trap";
    /// `(addr, len, store) -> offset`: the window offset of a load (store
    /// non-zero: a store) of `len` bytes at guest `addr`, which isn't RAM
    /// through KUSEG or KSEG0 ([`super::map::FAST_MASK`]): the console's
    /// map ([`super::map::access`]). A bus error traps.
    pub const MAP_ACCESS: &str = "map_access";
    /// `(fp) -> index`: the dispatch-table index of a function pointer
    /// outside RAM's KUSEG and KSEG0 view ([`super::map::code_index`]);
    /// traps where no code can be.
    pub const MAP_CODE: &str = "map_code";
    /// `(addr) -> ()`: a watched function was entered.
    pub const PROBE: &str = "probe";
    /// `() -> ()`: ReturnFromException (B0h:17h) is about to unwind the
    /// guest to `guest_invoke_irq`; the runtime notes it, or traps outside
    /// an exception handler.
    pub const RETURN_FROM_EXCEPTION: &str = "return_from_exception";
    /// `(fmt, va) -> ()`: a mod's `psxmod_log(fmt, ...)`: printf-style,
    /// the arguments in clang's va-buffer at guest address `va`.
    pub const MOD_LOG: &str = "mod_log";
    /// `(id, addr, a0, a1, a2, a3) -> i64`: an SDK function the build
    /// replaced with runtime adapter `id`, at guest address `addr`. The
    /// adapter finds its object's data through the relocated operands of
    /// the original code there (D61).
    pub const ADAPTER: &str = "adapter";
    /// `(addr, name) -> !`: a call into an overlay region found none of
    /// the code built for `addr` resident (D69); `name` is the guest
    /// address of the NUL-terminated name of the container resident
    /// there, or 0.
    pub const NOT_RESIDENT: &str = "not_resident";
    /// Adapters are exported as `adapter.<function name>` with the o32
    /// signature `(a0, a1, a2, a3) -> i64`.
    pub const ADAPTER_PREFIX: &str = "adapter.";
}

/// The runtime's view of the guest: module `guest`.
pub mod guest {
    pub const MODULE: &str = "guest";
    pub const MEMORY: &str = "memory";
    /// Mutable i32 global: guest stack pointer (the PSX `$sp`).
    pub const SP: &str = "__guest_sp";
    /// Mutable i32 global: the PSX `$gp`.
    pub const GP: &str = "__guest_gp";
    /// `(addr, a0, a1, a2, a3) -> i64`: call a guest function pointer
    /// through the dispatch table (boot, callbacks).
    pub const INVOKE: &str = "guest_invoke";
    /// `(addr, a0, a1, a2, a3) -> i64`: the same, in exception context: a
    /// ReturnFromException in the call unwinds to here (wasm `throw`), and
    /// `$sp`/`$gp` are restored to their values at the call.
    pub const INVOKE_IRQ: &str = "guest_invoke_irq";
    /// `() -> ()`: the mods' data patches to the executable, called once
    /// the boot has loaded it, before its entry point runs (M11). Empty
    /// without mods.
    pub const MOD_INIT: &str = "psxmod_init";
    /// `(addr, len) -> ()`: the disc just wrote `len` bytes at guest
    /// address `addr`. The overlays' code there is checked again before
    /// it next runs (D69), and the mods' data patches to an overlay whose
    /// code is now resident and whose patched bytes that range covered are
    /// applied (M11).
    pub const MOD_LOADED: &str = "psxmod_loaded";
    /// `() -> i32`: the window's wasm address ([`super::WINDOW_BASE`] or
    /// [`super::DEBUG_WINDOW_BASE`]). Hosts read it from the module's
    /// exports; a module without it has the default.
    pub const WINDOW: &str = "guest_window";
    /// `() -> ()`: the safepoint budget starts over, as at a safepoint. For
    /// adapters that replace a busy-wait (D59, D60): the guest resumes
    /// with the budget the original's last safepoint would have left it.
    pub const BUDGET_RESET: &str = "guest_budget_reset";
    /// `() -> i32`: the safepoint budget left, in [`BUDGET_UNIT`]s: at or
    /// below 0, the next loop header takes a safepoint. With
    /// [`BUDGET_SET`], `(i32) -> ()`, for adapters that replace code
    /// (D63, D71): they spend what the original's instructions would.
    pub const BUDGET_GET: &str = "guest_budget_get";
    pub const BUDGET_SET: &str = "guest_budget_set";
    /// The safepoint budget's unit (D71): a sixteenth of one of the
    /// original's instructions. Lifted code spends each MIPS block's
    /// instructions in it; decompiled C its estimate, which needs the
    /// fraction.
    pub const BUDGET_UNIT: i32 = 16;
    /// What an instruction takes on average (D71). Unverified: chosen
    /// because one cycle made libetc's `VSync` countdown (1 << 15 passes of
    /// 14 instructions) run out before a frame, not derived from the spec
    /// (psx-spx: an instruction's cycles depend on its class and on the
    /// region it accesses) nor measured against the oracle. Don't tune it.
    pub const CYCLES_PER_INSTRUCTION: i32 = 2;
    /// The budget a safepoint quantum gives: the default quantum's 20,000
    /// cycles of instructions.
    pub const BUDGET: i32 = 20_000 / CYCLES_PER_INSTRUCTION * BUDGET_UNIT;
    /// The global lifted code's spends name (psxw-lift's `psxw_rt.h`): every
    /// link resolves it to its budget.
    pub const BUDGET_SYMBOL: &str = "psxw_budget";
    /// binaryen Asyncify's control functions (D57): `(data_ptr) -> ()`
    /// start an unwind or a rewind over the data structure at `data_ptr`
    /// (current position, end, then the saved frames); `() -> ()` stop
    /// them; `() -> i32` is the state (0 normal, 1 unwinding, 2
    /// rewinding). A guest that never switches threads exports stubs.
    pub const ASYNCIFY_START_UNWIND: &str = "asyncify_start_unwind";
    pub const ASYNCIFY_STOP_UNWIND: &str = "asyncify_stop_unwind";
    pub const ASYNCIFY_START_REWIND: &str = "asyncify_start_rewind";
    pub const ASYNCIFY_STOP_REWIND: &str = "asyncify_stop_rewind";
    pub const ASYNCIFY_GET_STATE: &str = "asyncify_get_state";
}

/// What `game.wasm` imports from its host: module `host`. All pointers are
/// wasm addresses in the runtime's memory.
pub mod host {
    pub const MODULE: &str = "host";
    /// `(level, ptr, len) -> ()`.
    pub const LOG: &str = "log";
    /// `(code, ptr, len) -> !`: fatal, with a message.
    pub const TRAP: &str = "trap";
    /// `(frame: i64) -> ()`: vblank `frame` happened. Never suspends.
    /// Everything a host does at a vblank that the guest can see happens
    /// here (the frame counter replay input is keyed on, mod swaps,
    /// queued calls); it may trap to stop a run.
    pub const VBLANK: &str = "vblank";
    /// `(frame: i64) -> ()`: pacing after vblank `frame`: the host may
    /// sleep, present and suspend the game (JSPI), but changes nothing the
    /// guest can see. The runtime holds these calls while JS is on the
    /// stack ([`super::exports::JS_ENTER`]) and makes them, in order, once
    /// it has left.
    pub const WAIT_VBLANK: &str = "wait_vblank";
    /// `(out_ptr) -> i32`: writes [`DiscInfo`](super::DiscInfo); 0 = no disc.
    pub const DISC_INFO: &str = "disc_info";
    /// `(lba, count, mode, dst_ptr) -> i32` status (0 = ok); mode 0 = raw
    /// 2352-byte sectors, 1 = 2048-byte user data.
    pub const DISC_READ: &str = "disc_read";
    /// `(port, out_ptr) -> i32` bytes written: the pad's state this frame.
    pub const PAD_STATE: &str = "pad_state";
    /// `(name_ptr, name_len, dst, cap) -> i32` bytes read, or -1.
    pub const STORAGE_READ: &str = "storage_read";
    /// `(name_ptr, name_len, src, len) -> i32` status.
    pub const STORAGE_WRITE: &str = "storage_write";
    /// `(event_ptr, len) -> ()`: a structured run-report event (JSON).
    pub const REPORT: &str = "report";
    /// `(ptr, frames) -> ()`: audio since the previous call, at
    /// [`AUDIO_HZ`](super::AUDIO_HZ): `frames` interleaved little-endian
    /// i16 pairs (left, right). Called at each vblank with exactly the
    /// samples that fell due in virtual time.
    pub const AUDIO: &str = "audio";
    /// `() -> i32`: asked at a vblank, while the run records SPU windows
    /// ([`BootOptions::TRACE_RECORD_SPU`](super::BootOptions)) and none is
    /// open: how many samples to record from now, 0 for none.
    pub const SPU_RECORD_BEGIN: &str = "spu_record_begin";
    /// `(ptr, len) -> ()`: a finished window, little-endian `u32`s: its
    /// samples; the count of (register offset, value) pairs that recreate
    /// the SPU's register state at its start, then the pairs; the count of
    /// (sample, register offset, value) writes during it, then the writes;
    /// then SPU RAM at its start (512 KiB). A window in which SPU RAM
    /// changed (DMA4, the data port) is never sent.
    pub const SPU_RECORD: &str = "spu_record";
}

/// What `game.wasm` exports.
pub mod exports {
    pub const MEMORY: &str = "memory";
    /// `() -> i32`.
    pub const ABI_VERSION: &str = "psxw_abi_version";
    /// `(options_ptr) -> ()`: boot the disc and run the game until the host
    /// stops it (a trap from `vblank`). `options_ptr` points at
    /// [`BootOptions`](super::BootOptions) in memory the host wrote through
    /// `psxw_alloc`.
    pub const BOOT: &str = "psxw_boot";
    /// `(len) -> ptr`: scratch memory for the host to pass data in.
    pub const ALLOC: &str = "psxw_alloc";
    /// `() -> i64`: FNV-1a of guest RAM.
    pub const RAM_HASH: &str = "psxw_ram_hash";
    /// `(addr, len, store) -> ptr`: the wasm address of the `len` bytes a
    /// load (store non-zero: a store) at guest `addr` reaches, all in one
    /// region with bytes behind it ([`map`](super::map)); 0 otherwise.
    pub const GUEST_PTR: &str = "psxw_guest_ptr";
    /// `() -> i64`: the SPU's state (registers, voices, SPU RAM) brought
    /// up to now, hashed; for conformance (D63).
    pub const SPU_HASH: &str = "psxw_spu_hash";
    /// `(options_ptr, sp, gp) -> ()`: set the runtime up without a disc,
    /// for a host that drives guest calls itself (conformance runs). The
    /// host has written guest RAM.
    pub const CONFORM_INIT: &str = "psxw_conform_init";
    /// `(addr, a0, a1, a2, a3) -> i64`: call a guest function (o32), then
    /// run the callbacks it left queued; `v0 | v1 << 32`.
    pub const CALL: &str = "psxw_call";
    /// `(id, addr, a0, a1, a2, a3) -> i32`: queue one o32 call, safe while
    /// the game is suspended in `host.wait_vblank` or inside `host.vblank`. The driver executes it
    /// outside a machine borrow, interrupt handler or critical section.
    /// Returns 0 if accepted, -1 before init, -2 if busy, -3 for an invalid
    /// address. Completion: `host.report` JSON `{ "event": "execute",
    /// "id": N, "value": "<decimal u64>" }`. A trapping call stops the game.
    pub const EXECUTE: &str = "psxw_execute";
    /// `(n) -> ()`: let `n` vblanks of virtual time pass, delivering events
    /// and running interrupt handlers and callbacks.
    pub const RUN_VBLANKS: &str = "psxw_run_vblanks";
    /// `() -> ()`: a call from wasm into JS begins (a script mod's shim).
    /// While any is open, `host.wait_vblank` calls are held, since JSPI
    /// can't suspend through a JS frame.
    pub const JS_ENTER: &str = "psxw_js_enter";
    /// `() -> ()`: that call has returned (or unwound). When none is open
    /// any more, the held `host.wait_vblank` calls are made, in order,
    /// before this returns.
    pub const JS_LEAVE: &str = "psxw_js_leave";
}

/// Boot options, as the host writes them (little-endian `u32` fields).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BootOptions {
    /// 0 = realistic CD timing, 1 = instant.
    pub cd_timing: u32,
    /// 0 = NTSC, 1 = PAL.
    pub video: u32,
    /// Instructions of guest loop iterations between safepoints.
    pub safepoint_budget: u32,
    /// CPU cycles of virtual time per safepoint.
    pub safepoint_quantum: u32,
    /// Bit 0: trap on I/O-window accesses; bit 1: verify resident overlays;
    /// bit 2: report I/O-window bytes no device access wrote (`shadow-io`
    /// events, at each vblank).
    pub debug: u32,
    /// Bit 0: report every BIOS and adapter call; bit 1: offer GPU draw
    /// batches for recording; bit 2: offer SPU windows for recording.
    pub trace: u32,
}

impl BootOptions {
    pub const DEBUG_IO: u32 = 1;
    pub const DEBUG_OVERLAYS: u32 = 2;
    pub const DEBUG_SHADOW_IO: u32 = 4;
    pub const TRACE_CALLS: u32 = 1;
    pub const TRACE_RECORD_GPU: u32 = 2;
    pub const TRACE_RECORD_SPU: u32 = 4;
}

/// Disc layout, as `disc_info` writes it.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscInfo {
    /// Number of tracks (1-based track `n` starts at `track_lba[n - 1]`).
    pub tracks: u32,
    /// Total sectors on the disc.
    pub sectors: u32,
    pub track_lba: [u32; 99],
    /// 1 = data (mode 2), 0 = audio, per track.
    pub track_data: [u8; 99],
}

impl Default for DiscInfo {
    fn default() -> Self {
        Self { tracks: 0, sectors: 0, track_lba: [0; 99], track_data: [0; 99] }
    }
}
