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
pub const ABI_VERSION: u32 = 12;

/// The audio output rate: the SPU's, one sample per 768 CPU cycles.
pub const AUDIO_HZ: u32 = 44_100;

/// wasm address of guest physical address 0. Folded into every guest
/// load/store's `memarg` offset, so translation costs one `i32.and`.
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

/// Applied to every guest effective address. Folds KUSEG/KSEG0/KSEG1 onto one
/// physical view and keeps the scratchpad (`0x1F80_0000`) inside the window.
pub const WINDOW_MASK: u32 = 0x00FF_FFFF;

/// The runtime is linked with `--global-base` here; everything Rust owns
/// lives above the default guest window (below a debug build's, its heap
/// above it).
pub const RUNTIME_BASE: u32 = 0x0200_0000;

/// The kernel's tables in guest RAM (D57, M13): what the BIOS lays out
/// for games to read and edit directly (psx-spx "BIOS Memory Map"). The
/// rest of the kernel's state stays in the runtime. Addresses are
/// physical; the BIOS points at them through KSEG1, as the console's does.
pub mod kernel {
    use core::ops::Range;
    /// The table of tables: a (pointer, size) pair per table from 0x100.
    /// These two entries are filled: the process control block, and the
    /// thread control blocks.
    pub const PCB_ENTRY: u32 = 0x108;
    pub const TCB_ENTRY: u32 = 0x110;
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
    /// Kernel RAM a game may read and write at constant addresses: the
    /// lift's kernel rule accepts these and still fails on the rest.
    pub const RAM_BACKED: [Range<u32>; 2] = [PCB_ENTRY..TCB_ENTRY + 8, PCB..TCBS + MAX_TCBS * TCB_SIZE];

    /// Whether physical address `addr` lies in [`RAM_BACKED`].
    pub fn ram_backed(addr: u32) -> bool {
        RAM_BACKED.iter().any(|r| r.contains(&(addr & 0x1FFF_FFFF)))
    }
}

/// Guest physical layout inside the window.
pub mod phys {
    pub const RAM: core::ops::Range<u32> = 0x0000_0000..0x0020_0000;
    /// clang-only data (string literals, constant pools) with no original
    /// address. Occupies the region where hardware mirrors RAM.
    pub const AUX_DATA: core::ops::Range<u32> = 0x0020_0000..0x0060_0000;
    /// Mods' globals, stacks and heaps (M11), above AUX in RAM's mirror;
    /// never the game's RAM. A static build's mods share it, its stack at
    /// the top ([`MOD_STACK`]); dynamically loaded mods each get a slice.
    pub const MOD_DATA: core::ops::Range<u32> = 0x0060_0000..0x0080_0000;
    /// The stack static mods run on, at the top of [`MOD_DATA`].
    pub const MOD_STACK: core::ops::Range<u32> = 0x007F_0000..0x0080_0000;
    pub const SCRATCHPAD: core::ops::Range<u32> = 0x0080_0000..0x0080_0400;
    /// Hardware registers. Nothing backs them; debug builds trap here.
    pub const IO: core::ops::Range<u32> = 0x0080_1000..0x0080_3000;
    /// Runtime-private page: GTE register file, safepoint budget.
    pub const RUNTIME_PRIVATE: core::ops::Range<u32> = 0x00F0_0000..0x0100_0000;
}

/// Synthetic MIPS addresses for functions that have no original address
/// (mod functions whose address is taken or stored in game data).
pub const SYNTHETIC_CODE: core::ops::Range<u32> = 0x8020_0000..0x8040_0000;

/// Dispatch table index for a guest function pointer. The table spans
/// RAM plus the synthetic range; unmapped entries trap with the address.
#[inline]
pub const fn table_index(fp: u32) -> u32 {
    (fp & 0x003F_FFFC) >> 2
}
pub const TABLE_LEN: u32 = 0x0010_0000;

/// Translate a guest pointer to its wasm address (the default window).
#[inline]
pub const fn guest_to_wasm(addr: u32) -> u32 {
    (addr & WINDOW_MASK) + WINDOW_BASE
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
    /// `(table, fn, a0, a1, a2, a3) -> i64`: BIOS vector call.
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
    /// address `addr`; the mods' data patches to an overlay whose code is
    /// now resident and whose patched bytes that range covered are applied
    /// (M11). Empty without mods.
    pub const MOD_LOADED: &str = "psxmod_loaded";
    /// `() -> i32`: the window's wasm address ([`super::WINDOW_BASE`] or
    /// [`super::DEBUG_WINDOW_BASE`]). Hosts read it from the module's
    /// exports; a module without it has the default.
    pub const WINDOW: &str = "guest_window";
    /// `() -> ()`: the safepoint budget starts over, as at a safepoint. For
    /// adapters that replace a busy-wait (D59, D60): the guest resumes
    /// with the budget the original's last safepoint would have left it.
    pub const BUDGET_RESET: &str = "guest_budget_reset";
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
