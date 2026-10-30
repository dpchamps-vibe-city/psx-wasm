# psx-wasm: design

This document is the source of truth for what psx-wasm builds and why. The
body states what holds now; the [decision log](#decision-log) at the end
records each decision and its reasons. Plans in [`plans/`](../plans/README.md)
say how and in what order.

Status (2026-10-29): **M0–M8 and M11–M13 are implemented, and M10 as far
as it goes without a rig; two games build and run, btn and bof4.** The
Roadmap lists what stays open. The input measurements below and the
M0/M1 results were taken against `btn-decompilation@3a15992` (working tree
dirty) and `psyq-decomp@302b6ae` on 2026-10-25/26.

## Goal

Take a matching PlayStation decompilation of a game, plus a decompilation of
the PsyQ SDK it was built with (which may be partial), and produce a
WebAssembly build that runs the game natively in a browser. The game's code
is compiled to wasm. Below the SDK, Rust models the hardware at register
level, on a deterministic virtual clock. There is no interpreter, no
dynarec and no CPU model, and the devices are not modelled cycle by cycle:
DMA and drawing complete instantly in virtual time.

The first target is Harvest Moon: Back to Nature (SLUS-01115) with PsyQ 4.7.
The test of generality is that a second decompilation runs with only a
manifest, without changes to the toolchain's code.

## What "native, not an emulator" means here

| PSX part | In psx-wasm |
|---|---|
| R3000 CPU | **Gone.** clang compiles the game and SDK C to wasm. Any residual asm is translated ahead of time (the lift tier). Nothing decodes MIPS at run time. |
| Memory map | **Kept.** RAM, the scratchpad, and every symbol sit at their original addresses inside a wasm memory window, and pointer values are the PSX's own. This is a choice of data layout, not hardware emulation. |
| GTE | **Modelled.** A fixed-point library (`psx-gte`) holding the GTE's register file, which COP2 instructions and the `gte_*` macros drive. |
| GPU | **Native, per chip** (D59, D61). libgpu's `sys.o` adapters (btn, bof4) give the GPU front end (`psx-devices::gpu`) its GP0/GP1 words and run its transfers on the DMA controller the other chips share; libgpu's state stays in guest RAM, where the library's code that stays original reads it. As the fallback, libgpu is original code over the front end, which answers GPUSTAT/GPUREAD, fed by the DMA controller (D42). The front end decodes packets into renderer commands; the host's WebGL2 backend draws them into a VRAM model. |
| SPU | **Native, per chip** (D59, D63). libspu's functions that touch the SPU (`spu.o`'s reset, register and transfer layer, the voice, key, mask, common and reverb functions) and libsnd's `_SsInit` are adapters (btn and bof4) over `psx-spu`'s mixer API, its DMA4 transfers on the shared DMA controller with their completions held to the guest's next safepoint; libspu's and libsnd's state stays in guest RAM, so the rest of libspu and all of the libsnd sequencer are original code over them. The core: 24 ADPCM voices with reverb, on their own 44.1 kHz clock. As the fallback, libspu and libsnd are original code over the SPU's registers and DMA4 (D47). |
| CD-ROM | **Native, per chip** (D59, D62). libcd's drive layer (`CD_init`, `CD_cw`, `CD_sync`, `CD_ready`, `CD_getsector`…) and the streaming ring's interrupt are adapters (btn and bof4) over the CD controller's core (`psx-devices::cdc`: the user's disc image, ISO9660, the drive's timing, XA into the SPU's CD input), driven without its register protocol; libcd's public layer, libds and the rest of streaming are original code over them, their state where the original keeps it. As the fallback, libcd runs as original code over the same controller's registers (D58). |
| MDEC | **Native, per chip** (D59, D64). libpress's adapters (bof4, the game that links it) give the MDEC's decode core (`psx-devices::mdec`) the run-length codes `DecDCTin` names and put its pixels into `DecDCTout`'s buffer, without its register protocol. DMA0 and DMA1 complete on the shared DMA controller, so their callbacks come through libetc's DMA handler, at the guest's next safepoint as on the device. As the fallback, libpress is original code over the MDEC's registers, DMA0 in and DMA1 out (D58). |
| Pad | **Native** (D59, D60). libpad's adapters run its direct-connection engine (btn and bof4): at each vblank interrupt, one exchange per port with a digital-pad model fed by the host's buttons, the detection steps and the buffers as libpad's own state machine makes them, and the time its busy-waits take. As the fallback, libpad is original code over SIO0 (`psx-devices::sio`, D40). |
| Interrupts, timers, DMA | **Native, per chip** (D59). libetc's and libapi's counter adapters run its interrupt layer (btn and bof4): after the BIOS's `SysEnqIntRP` chains, their dispatcher calls each pending line's handler, over the register-level interrupt controller, root counters and DMA controller that the other chips' devices share. As the fallback, libetc is original code over those devices, and the BIOS enters its dispatcher through its custom exception exit (D45). Interrupts are delivered at safepoints, on a deterministic virtual clock. |
| BIOS/kernel | A Rust implementation of the A0/B0/C0 services the SDK calls, including threads (D57) and the memory card functions (D43). |

So the CPU is not emulated: all code is compiled ahead of time. The
hardware below the device libraries is emulated, at register level.
D4 and D8 placed the boundary at the SDK's API, with an adapter for every
SDK function that touches hardware. D40–D58 moved it down to the registers,
library by library, because the original library code then decides
everything a game can observe, exactly and for any SDK version. Adapters
remain where a library's policy keeps them (§5).
[`emulation-layers.md`](emulation-layers.md) draws the layers, and
[`status-btn-vs-bof4.md`](status-btn-vs-bof4.md) compares the two games.
D59 moves each chip back to native adapters, degrading to its device
model where the adapters don't cover a game
([`plans/unified-adaptor-layer/`](../plans/unified-adaptor-layer/README.md)):
every chip but the GTE in both games: the CD (chip cd), the interrupt
layer (chip irq), the GPU (chip gpu), the pads (chip sio0) and the SPU
(chip spu), and the MDEC (chip mdec) in bof4, the game that links
libpress.

## What the inputs look like (measured)

**Game: btn-decompilation**
- The build has 14 containers: the PS-X EXE and 13 overlays. Together they
  hold 2,565 live functions (651,560 bytes) and match the original byte for
  byte.
- The overlays load into two slots, `0x800B7E20` and `0x8012DDE8`. The EXE
  calls into them through 690 absolute-address imports
  (`build/named_overlay_imports.ld`).
- The source is 1,798 `.c` files (about 96k lines). About 49 functions are
  still `INCLUDE_ASM`, among them `__start`, `PresentFrame` and the GTE
  packet emitters.
- 18 files contain inline asm. Most of it is register pinning
  (`register x __asm__("$11")`) and empty `""` barriers. A few sites are real
  instructions, such as `lw %0,32($sp)`.
- The only hardware addresses written in C are a handful of references to
  the scratchpad (`0x1F800000`). There are no hard-coded `0x80xxxxxx`
  pointer casts.
- The game links 361 SDK archive members (147 KB of code): libsnd 106,
  libapi 39, libspu 33, libgte 30, libmcx 27, libcd 22, libgpu 21, libc 18,
  libmath 17, libgs 15, libcard 12, libpad 8, libmcrd 6, libetc 5.
- It does not link libpress, so there is no MDEC video. It does use XA
  audio.
- The interrupt-driven SDK functions it links are `VSyncCallback`,
  `DrawSyncCallback`, `CdReadyCallback`/`CdSyncCallback`,
  `InterruptCallback`, the root counters (`SetRCnt`, `StartRCnt`,
  `GetRCnt`), events, `SsSetTickMode`/`SsSeqCalledTbyT`, `setjmp` and
  `FlushCache`.

**SDK: psyq-decomp.** The SDK decompilation is far from complete. This
table counts source files that contain no `INCLUDE_ASM`, out of all source
files:

| Library | Files with no asm | Library | Files with no asm |
|---|---|---|---|
| libgs | 420 / 639 | libsnd | 31 / 540 |
| libgpu | 125 / 201 | libspu | 9 / 312 |
| libcd | 78 / 163 | libgte | 5 / 288 |
| libapi | 29 / 49 | libpad | 2 / 103 |
| libetc | 26 / 43 | libmath | 0 / 150 |

**What this means for the design.** The toolchain cannot assume that SDK C
exists:
- Translation covers every library (D15). The libraries that touch
  hardware mostly run as original code over the devices, translated where
  there is no C, so their decompilation state doesn't gate anything.
- libsnd is the largest library the game links, and it is mostly asm. The
  game's ~49 `INCLUDE_ASM` functions sit on the render path.
- So the lift tier (below) is on the critical path to the first frame. It is
  not an optional extra.

## Core decisions

### 1. Keep the PSX address space

wasm32 is unusually well suited to this. Its pointers are 32 bits and
little-endian, and its C struct layout (alignment of `int`, `long`,
`long long` and `double`, and bitfield order) agrees with MIPS o32. So psx-wasm
keeps the original memory map instead of letting a wasm linker lay memory
out:

- **Data placement.** Every data symbol is placed at its original address,
  taken from the matching build's ELFs and link maps.
- **Pointer values.** Pointers keep their PSX values (`0x80xxxxxx`) in
  registers and in memory. Only the *effective address* of a load or store
  is translated:

  ```
  ea = (addr & 0x00FF_FFFF) + WINDOW_BASE
  ```

  The `+ WINDOW_BASE` folds into the wasm `memarg` offset immediate, so each
  guest memory access costs one `i32.and`. The same mask folds KSEG0, KSEG1
  and KUSEG onto one physical view and puts the scratchpad at window offset
  `0x800000`.
- **Initial memory comes from the disc.** At boot the runtime loads the
  PS-X EXE from the user's disc into RAM, as the BIOS does. Overlays arrive
  the way the game always loaded them, through its own CD reads. The wasm build
  therefore contains code and an address map, and no game data.
- **A free build-time check.** For every global that C defines, the
  initializer clang produces must equal the original bytes at that address.
  This checks struct-layout compatibility for every global in the game.

What this buys:
- No source patches for address arithmetic.
- Assets that contain absolute pointers work unchanged.
- Code that checks pointer ranges (`p >= 0x80100000`) still works.
- Overlays that share data addresses still alias the way they did on the
  PSX.

| wasm address | Contents |
|---|---|
| `0x0000_0000`–`0x00FF_FFFF` | Unused. This keeps null out of both the guest and the runtime. |
| `0x0100_0000`–`0x01FF_FFFF` | **Guest window.** Window offsets `0x000000`–`0x1FFFFF` are RAM, `0x200000`–`0x5FFFFF` hold auxiliary data (literals and constant pools that only clang emits), `0x600000`–`0x7FFFFF` hold mods' data and stacks (M11), `0x800000` is the scratchpad, `0x801000`–`0x802FFF` are the I/O registers (below), and `0xF00000` is a runtime-private page (the GTE register file). |
| `0x0200_0000`– | The runtime's own data, stack and heap, placed there by the linker. |

A debug build moves the window to `0x8000_0000` (§9).

**The I/O window.** psxw-link wraps each load and store of a device
library's functions (§5) with a range check on the masked address
(`rewrite::io_call`). An access in the I/O range calls the runtime's bus,
`psx.io_access` (`psx-bios::io`): a load calls it before reading, so the
device puts the register's value in place, and a store calls it after
writing, so the device takes the value. clang merges adjacent register
accesses into wider and unaligned ones (lifted libspu sets four voice
registers with one 8-byte store), so the bus splits each access into the
naturally aligned registers it covers, a word at most, in address order
(`Machine::io_access`). Any other function's access reads
and writes plain memory, the window's *I/O shadow*. A `--debug-io` build
traps on those (`psx.io_trap`), and `psxw run --check no-shadow-io`
reports the first frame where a shadow byte changed without a device
access.

The guest stack is the PSX stack. clang's shadow stack pointer for guest
code is renamed to a separate `__guest_sp` global, initialized from the EXE
header, so it never collides with the Rust stack. Lifted code uses the same
global as `$sp`.

### 2. Function pointers are MIPS addresses

Taking the address of a function yields its original MIPS address, the same
number the game would see on hardware. Every indirect call goes through one
dense funcref table:

```
call_indirect $dispatch ((fp & 0x003F_FFFC) >> 2)
```

- **One indirect signature.** Every table entry has the same wasm type, the
  *o32 indirect ABI*: `(a0, a1, a2, a3: i32) -> i64`. The result packs `v0`
  and `v1`. Arguments 5 and up go on the guest stack at `sp+16`, as in o32.
- **Adapters.** The linker generates them from prototypes, converting
  between this uniform form and each function's real wasm signature.
- **Interop with lifted code.** Lifted code makes indirect calls with
  exactly this convention, so C and lifted functions interoperate through
  function pointers with no special cases.
- **Mismatched prototypes are harmless.** C89 decompilations call
  unprototyped and mismatched function pointers constantly. Normally wasm
  traps on these. Under o32 semantics they are harmless: extra arguments are
  ignored and missing ones read as garbage, as on hardware.
- **Functions with no MIPS address.** Compiler-generated helpers whose
  address is taken get synthetic addresses above RAM, in window offsets
  `0x200000`–`0x3FFFFF`.

### 3. Overlays: the bytes in RAM decide

Each overlay's functions are compiled into `game.wasm` alongside the EXE.
An address that more than one overlay defines gets a dispatch stub:

1. Read the first word or two of guest RAM at that address.
2. Compare them against each candidate overlay's original code at that
   address.
3. Call the candidate that matches. If none matches, trap with a diagnostic
   ("no translation for resident code at 0x800BBC74").

The 690 EXE→overlay imports, and every table entry for an overlay address,
point at these stubs.

Residency is decided by RAM contents, the same way the hardware decides
which code runs. It doesn't matter how the bytes arrived: CD DMA, a game
`memcpy`, or decompression. No per-game loader hook is needed. Calls inside
an overlay are direct and unchecked. Caching the check against a
region-write epoch is a later optimization.

### 4. Every function gets exactly one implementation

Each function at each guest address is implemented exactly once. `psxw`
applies one rule per function, game and SDK alike:

1. **Adapter**, if the runtime has one for the function, its library's
   policy binds adapters, and its object is one the adapter is validated
   against (§5). An adapter is a thin Rust function in the runtime.
2. Otherwise **decompiled C**, if the decomp has it and it compiles under
   the host prelude.
3. Otherwise **translated**: the original MIPS, translated ahead of time
   into C by `psxw-lift` and compiled by the same clang pipeline.

A device library's functions are always C or translated; their I/O
accesses reach the runtime's register-level devices (§1, §5).

| Tier | Source | Produced by |
|---|---|---|
| `adapter` | Rust, in the runtime | Chosen by the library's policy in `boundary/psyq.toml` and the object's identity |
| `c` | Decompiled C | `psxw-cc` |
| `lift` | Original MIPS from relocatable objects, translated ahead of time to C | `psxw-lift`, then `psxw-cc` |

How each tier is produced:

- **C tier.** A prelude, injected with `-include`, neutralizes MIPS-isms.
  The normalizer strips register-pinning asm and recognizes any splat-style
  `INCLUDE_ASM` expansion as a lift marker. The `gte_*` macros map onto
  `psx-gte`. A function falls to the lift tier when its C can't say what
  the original does:
  - it contains real inline asm or a GNU nested function;
  - it uses a GNU global register variable, or reads a local register
    variable it never assigns (a value carried in a MIPS register across
    calls);
  - it can fall off the end of a non-void function (the original returns
    whatever is in `v0`);
  - it does floating-point arithmetic and the original was built
    `-msoft-float` (its results come from the game's own soft-float
    routines).

  Every non-inline function compiles `weak` and `noinline` (D50), so each
  call the original makes stays a call, as GCC 2.95 at `-O2` left it: a
  mod's hook sees it, and clang's inlining doesn't move where loops spend
  the safepoint budget.

  At link time, C inputs also reproduce what GCC 2.95 did that clang doesn't
  (the `Gcc` conventions): divisions give the R3000's results and the
  original assembler's `break` checks instead of trapping; `memcpy` with a
  constant size is expanded inline (builtins were on); `main` starts with a
  call to `__main`.
- **Lift tier: generated C.** The translator works one function at a time.
  For a game, it reads the **original container images** directly: the
  address space is kept exactly, so absolute addresses in placed code are
  already valid. Calls resolve through the address map, and jump tables are
  bounded by analysis. No relocatable objects, and no build of the decomp
  repo, are needed. Relocatable `.LIB` members are reserved for the
  reusable translated-SDK cache (below). The translator emits C, the
  approach N64Recomp and PSXRecomp use, rather than wasm directly:
  - **One pipeline.** Generated C goes through the same clang flags,
    prelude and guest memory model as decompiled C.
  - **One calling convention for generated code.** Every generated
    function has the o32 form `u64 f(a0, a1, a2, a3)`: the result packs
    `v0 | v1 << 32`, and further arguments go on the guest stack. Wherever C
    and generated code call each other, the linker inserts an adapter
    derived from the C side's wasm signature. Prototype facts from
    `psxw-cc` cover structs passed by value and variadic calls. So a
    function can move from translated to decompiled C without its callers
    changing.
  - **clang optimizes it.** Guest registers become locals that clang
    promotes to SSA form. `#line` directives point back at the original
    instructions for debugging.
  - **UB discipline.** The output uses wrapping unsigned arithmetic,
    explicit sign extension and masked shifts, so clang has no undefined
    behaviour to exploit.

  Delay slots are resolved statically, and load-delay
  hazards are modelled where a consumer sits in the slot. Other instructions
  are routed as follows:
  - COP2 maps onto `psx-gte`.
  - Jumps to the BIOS tables (`0xA0`/`0xB0`/`0xC0`, function number in
    `$t1`) and `syscall` become calls to the runtime's BIOS dispatcher.
  - COP0 and `break` become runtime calls.
  - Loads and stores at constant I/O addresses, and at kernel RAM the
    BIOS doesn't keep in guest RAM (D57), are recorded. Outside SDK
    objects they fail the lift. In a `device` library they reach the
    runtime's devices; in an `adapter` library, a function that makes them
    with no validated adapter fails the build if game code can reach it
    (§5).

  Generated code assumes o32 at function boundaries. Handwritten asm that
  passes values in non-argument registers (libgte's normalize helper takes
  and returns `t0`–`t2`) is handled by *inline expansion*, not a separate
  full-state mode. When a caller writes registers the callee reads on
  entry, or reads registers after the call that the callee writes, the
  callee is expanded into the caller with every register shared, and its
  `jr ra` dispatches over the known return sites. That is exactly the
  hardware's behaviour, and needs no register file in memory. Decompiled C
  is ABI-conformant by construction, so C-defined functions and their
  translations never inline.

  Function discovery runs to a fixed point. Starts are seeded from the
  inputs:
  - symbol files;
  - the first function of each object's `.text`;
  - the census's C definitions;
  - the EXE entry.

  They grow from call targets and code addresses built in registers. An
  address reachable inside an existing function becomes an interior entry
  instead of splitting it. Jump tables are recovered from the GCC idiom,
  including indices spilled to the stack. A constant read from the
  straight-line window must be confirmed by dataflow. Calls name their
  target by address (`psxw_f_<ADDR>`, or `psxw_d_<ADDR>` for a call into an
  overlay slot), and the linker binds each one to a tier.

  Hand-written code adds three idioms (M13):
  - a table of code addresses inside the function's own text, indexed
    without a bounds check: its entries are the words that point into the
    function, and it becomes a local switch;
  - a jump to a computed offset in a block only a code pointer reaches,
    which reads registers the ABI doesn't pass (an unrolled loop entered
    part way): the block is expanded into the jumping function, every
    instruction of its branch-free head a target;
  - a jump through a register holding the entry `ra`: `r31` starts at 0,
    standing for the caller's return address, so a jump to 0 returns.

  A load whose result nothing reads is kept, as a volatile read: from an
  I/O register it pops a FIFO.
- **A translated SDK is reusable.** Games link SDK members byte for byte, so
  the translated C can be cached per SDK release, keyed by member hash, and
  reused by every game linking that member. It is derived from Sony's
  binaries, so it would be built locally from the user's inputs and never
  distributed. The cache isn't built yet (M8 task 5); today each game
  translates its placed SDK code from its own images.
- **The tier report.** `psxw report tiers` counts each library's functions
  by tier, and `tiers.json` lists every function's. For game code, the lift
  tier is a migration aid that shrinks as the decompilations progress. The
  SDK's handwritten asm (BIOS trampolines, GTE wrappers, `mem*`) never
  becomes C, so translation is permanent there.

### 5. The SDK boundary: a policy per library

The game's matching link fixes the set of SDK functions. Each library the
game places gets a policy in the boundary file, `boundary/psyq.toml` (M8);
a library with no entry fails the build (M13). A per-object entry
(`[object."<library>/<member>"]`) overrides its library's policy.

| Policy | How the library's functions run |
|---|---|
| `adapter` | A function with a runtime adapter, in an object the adapter is validated against, runs the adapter. The rest are guest code. |
| `device` | Guest code. Its I/O-window accesses reach the runtime's register-level devices (D40). No adapters. |
| `adapter-or-device` | Decided per game for the member's whole chip group (D59), never per library or object: `adapter` when the chip runs native, otherwise `device`. Every chip library has it. |
| `guest` | Guest code only; no adapter may claim its functions. |

Guest code is decompiled C or translated, exactly like game code (§4).

**Chips (D59).** The boundary file groups the chip libraries: irq
(libetc, libapi's `counter.o`), gpu (libgpu), sio0 (libpad), cd (libcd,
libds), mdec (libpress), spu (libspu, libsnd). `psxw build` decides each
chip as a whole:
- **native** when every placed object of the group that defines an
  adapter's function is validated, the game binds adapters there, and the
  code outside the group, through direct calls that stop at those
  adapters, reaches nothing in the group that touches hardware or kernel
  RAM itself. Group code it reaches that doesn't (libds over libcd, a
  packet helper) runs as guest code over the adapters.
- **emulated** otherwise: every member's original code over the chip's
  register-level device.

What touches hardware comes from the translator's constant-address facts
and from the SDK's reach table, `boundary/reach/<sdk>.json`. `psxw sdk
scan` writes the table from the SDK's library objects; it sees register
pointers kept in data, which most PsyQ libraries use. `build.json` gives
each chip's mode and the reason, and a summary line ("emulated: gpu,
spu; 2 of 5 chips"). `game.toml` `[chips] native` lists the chips a game
expects native, and the build fails if one falls back. `--device
<chip,…|all>` forces chips onto their devices. A run reports register
accesses per chip, and `psxw run --check no-io` requires none.

**Where the boundary sits today.** Adapters drive every chip but the GTE
in both games: the interrupt controller and the root counters (chip irq),
the GPU (chip gpu), the pads (chip sio0), the CD (chip cd: libcd's drive
layer over the CD controller's core) and the SPU (chip spu: libspu's
lowest layer over the mixer API), and the MDEC (chip mdec: libpress over
its decode core) in bof4; no guest code touches a register
(`--check no-io`). The register-level devices stay, as the fallback and
as the conformance oracle. Each move below the API (D40, D42, D45, D47,
D58) bought exactness by construction and independence from the SDK
version, because the original library decides every value a game can
observe. Together they made the layer beneath the SDK an emulated device
layer ([`emulation-layers.md`](emulation-layers.md)). D59 makes the
adapters first again, chip by chip, with the device models as the
fallback (`plans/unified-adaptor-layer/`).

**The runtime splits into three layers**, so that SDK versions stay cheap:

```
 guest code (game + SDK code that runs as-is: libgpu, libetc, libpad,
             libspu, libsnd, libgs, libc …)
      │ calls a replaced           │ I/O register access         │ BIOS jump
      │ SDK function               │ (device libraries)          │ / syscall
      ▼                            │                             ▼
 SDK adapters (thin, versioned)    │                    BIOS (A0/B0/C0, syscall)
      └──────────────┬─────────────┴─────────────────────────────┘
                     ▼
 devices (hardware-defined, register level): irq, timers, dma, gpu, spu,
          sio0, cd drive and controller, mdec; plus gte and the clock
```

- **Devices** speak the hardware's own vocabulary: registers, GP0/GP1
  words, DMA blocks, interrupt lines. That vocabulary is the same for PsyQ
  3.x, PsyQ 4.7, PSn00bSDK or a custom engine. They sit on the bus in
  `psx-bios::io`, and interrupts run the kernel's `SysEnqIntRP` chains.
  Nearly all the hard work lives here.
- **The BIOS** is the console's ROM, so it is identical for every SDK
  version. libapi is translated like any other library: 33 of the 39
  members btn links are four-instruction trampolines (`li t2,0xB0; jr t2;
  li t1,N`). The translator routes them to the Rust BIOS dispatcher.
- **Adapters** are thin. Each one translates a particular version's PsyQ
  call into device and BIOS calls: unpacking that version's structs and
  return codes, and maintaining any SDK globals that guest code reads. What
  varies between versions (struct layouts, function sets, internal globals,
  bug fixes) is confined to them.

**Adapters are keyed by object identity, not by "the SDK version".**
- **Games mix versions.** A game may link libsnd from one release and
  libgpu from another, sometimes patched.
- **Every linked member is identified exactly.** `psxw identity <game>`
  matches placed text against a pinned signature database; the key is the
  sha256 of the text with relocated bytes masked.
- **Binding.** An adapter binds only to an object listed in sdk-psyq's
  `validated.rs` as (member, key). A function of any other object runs as
  guest code, cutting down to whatever lower functions do have adapters,
  down to the BIOS calls if necessary.
- **The build fails** if a function in an `adapter` library that itself
  touches I/O or kernel RAM is reachable from game code, through direct
  calls that never pass an adapter, with no validated adapter. The error
  names the functions; their objects need adapters or a `device` policy.
  Unreachable ones are listed in `build.json`.
- **Struct layouts are generated.** `psxw layout <game>` writes the
  structs adapters read and write (the boundary's `[layout]`) from the
  game's SDK headers, with offsets checked equal under the matching
  compiler and clang. The M0 layout probe showed clang's wasm32 layouts
  equal GCC's o32 layouts.
- **Adapters take the o32 form** `(a0, a1, a2, a3) -> i64` and are
  exported as `adapter.<name>`, so a caller in any tier reaches them the
  same way.
- **Other SDK families**, such as PSn00bSDK or Net Yaroze, would be sibling
  boundary files and adapter sets over the same devices and BIOS.

The PsyQ policy (`boundary/psyq.toml`):

| Library | Policy |
|---|---|
| libapi | `adapter`. Translated; BIOS calls go to the Rust BIOS (D13). Adapters for `pad.o` and `patch.o` (BIOS pad buffers and patches) and `first.o` (`firstfile`, the memory card directory). `counter.o` (`SetRCnt`… program the timers) is `adapter-or-device`, in chip irq. `chclrpad.o` and `padstop.o` touch no hardware and run as guest code. |
| libetc | `adapter-or-device`, chip irq. Native (btn, bof4): adapters for `intr.o`'s and `vsync.o`'s functions, their state in the runtime, over the interrupt controller, DPCR/DICR, root counter 1 and GPUSTAT, which stay the devices' registers because other libraries use them too ("Time, interrupts and blocking"). On the device: original code over the IRQ controller, DMA and timers (D45). The BIOS enters its dispatcher through the custom exception exit (a longjmp into `startIntr`'s `setjmp`), and it leaves with ReturnFromException; the lift and link tiers lower both to wasm exceptions. |
| libgpu | `adapter-or-device`, chip gpu. Native (btn, bof4): adapters for `sys.o`'s functions that reach the GPU (`ResetGraph`, `DrawSync`, `DrawOTag`, `PutDrawEnv`, `PutDispEnv`, the image transfers …), over the front end and DMA2/DMA6, with libgpu's state (`GEnv`, its command queue, its packets) where libgpu keeps it, found through the object's relocated code (D61). The rest of libgpu (the font and kanji functions, `DrawSyncCallback`, `ClearOTag`, `SetDrawEnv`, the primitive helpers) and libgs stay guest code over them. On the device: original code over the GPU front end and DMA (D42). The front end takes GP0/GP1 words, so libgpu's own code drives it; an ordering table is one DMA walk in Rust. |
| libcd | `adapter-or-device`, chip cd (D58, D62). Native (btn, bof4): adapters for the drive layer (`bios_1`'s `CD_init`, `CD_initintr`, `CD_cw`, `CD_sync`, `CD_ready`, `CD_flush`, `CD_vol`, `CD_initvol`, `CD_datasync`; `CD_getsector`, `CD_getsector2`) and the streaming ring's `StCdInterrupt` and `StUnSetRing`, over the CD controller's core and DMA3, their state in the library's own globals; the public layer (`CdInit`, `CdControl*`, `CdRead*`, `CdSearchFile`, the callbacks and accessors) and the rest of streaming run as original code over them. On the device: original code over the CD controller's registers (D58). |
| libds | `adapter-or-device`, chip cd. It touches no register: guest code over libcd's drive layer, adapted or original (D58, D62). |
| libpress | `adapter-or-device`, chip mdec. Native (bof4; D64): adapters for every `libpress.o` function that reaches the MDEC or its DMA (`DecDCTReset`, `DecDCTPutEnv`, `DecDCTin`, `DecDCTout`, `DecDCTinSync`, `DecDCToutSync`) over the MDEC's decode function, completing DMA0 and DMA1 on the shared controller. libpress's tables stay in its `.data` (D61), so `DecDCTGetEnv` and the callback setters stay guest code, as do the run-length coders (`DecDCTvlc*`). On the device: original code over the MDEC, DMA0 in and DMA1 out (D58). |
| libspu | `adapter-or-device`, chip spu. Native (btn, bof4; D63): adapters where libspu touches the SPU: `spu.o`'s `_spu_init`, `_spu_FsetRXX`, `_spu_FsetRXXa`, `_spu_Fw` (SPU RAM by DMA4 or the FIFO) and `_spu_FiDMA` (DMA4's callback), and the functions that program it themselves (`SpuSetKey`, `SpuSetVoiceAttr`, `SpuSetCommonAttr`, `_SpuSetAnyVoice`, the reverb functions, `SpuClearReverbWorkArea`, the key and envelope readers), over the mixer API (`psx_spu::mixer`). libspu's state stays in its `.data`, found through the objects' code (D61), so `SpuInit`, `SpuStart`, `SpuWritePartly`, `SpuSetTransferStartAddr`, `SpuIsTransferCompleted`, the allocator and the voice-mask wrappers stay guest code over them. On the device: original code over the register-level SPU and DMA4 (D41, D47). |
| libsnd | `adapter-or-device`, chip spu. Original code (D14): the sequencer *is* the game's music. Only `_SsInit` touches the SPU itself (16 registers from its table); natively it is an adapter (D63) that calls the original `_SsVmInit` and writes libsnd's globals where libsnd keeps them; the rest of libsnd reaches the SPU through libspu's adapters. On the device those writes reach the SPU (D47). |
| libpad | `adapter-or-device`, chip sio0. Its state machine is the behaviour games observe (D40). Native (btn, bof4; D60): adapters for `PadInitDirect`, `PadStartCom`, `PadStopCom`, `PadGetState`, `PadInfoMode`, `PadSetAct` and `PadSetActAlign` run its direct-connection engine (`pddirres.o`, `pdresres.o`), their state in the runtime, the games' buffers in guest RAM; `PadStartCom` installs a `SysEnqIntRP` element the kernel runs as an adapter step. On the device: original code over SIO0 (D40), a port of the oracle's. |
| libmcrd | `guest`, over the HLE BIOS card functions (D43). |
| libcard | `adapter`. Its `_patch_card` patches Sony BIOS code in kernel RAM (D43). |
| libmcx | `adapter`. |
| libgs | `guest`. It only writes packets into RAM; libgpu's `DrawOTag` is the device step. |
| libgte | `guest`, with COP2 through `psx-gte`. |
| libmath, libc, libc2 | `guest`. `rand` must be the original, or the game plays differently. |
| libsn, 2mbyte | `guest`: SN Systems' C++ runtime and startup code, which touch no hardware. |

D14 allows an adapter in a pure library for speed, where equivalence is
easy to show. None exists, and the `guest` policy refuses one.

**Pointer I/O outside device libraries.** Code outside device libraries
that reaches a register through a pointer writes only the window's I/O
shadow (§1); `psxw run --check no-shadow-io` catches that at the frame it
happens.

**Conformance.** `psxw conform <game> --lib L` runs one scenario of SDK
calls and vblanks, with no disc, on `game.wasm` and on `game-orig-L.wasm`,
where `L` is its original code over the devices. What the game can observe
after each step (return values, the SDK buffers it reads) must agree.
Scenarios exist for libpad, libgpu, libmcrd, libetc, libsnd and libspu.
The scenarios' own checks add to the comparison: libgpu's fill and tile
read back exactly, libspu's voice is audible, libsnd's tick runs at its
rate. The chip cases (`psxw conform <game> --chip C`, D59) compare each
chip's adapters with its device the same way, call by call, the device
effect at the core included (the renderer's commands, the displays, the
audio, the SPU's state).

### 6. Time, interrupts and blocking

Game-visible time is **virtual and deterministic**:
- **Safepoints move time.** psxw-link adds a decrement-and-test of a
  budget global to every loop header in guest code (1,000 iterations,
  set at link time; mods' loops don't spend it). When the budget
  runs out, the runtime advances virtual time by a quantum (20,000 CPU
  cycles by default, `BootOptions::safepoint_quantum`) and delivers what
  fell due.
- **Waiting is spinning.** libgpu's `DrawSync` is original code (D42),
  and so is libetc's `VSync` with chip irq on its device (D45): they wait
  the way they do on hardware, by polling. `VSync(0)` spins until libetc's
  vblank handler, delivered at one of the loop's safepoints, advances
  `Vcount`. A spin loop like `while (!cd_done);` makes progress the same
  way. An adapter that replaces a spin waits the way the spin does (D59):
  `VSync`'s adapter moves time a quantum at a time, delivering what fell
  due and then running the handlers, as at the loop's safepoints. When
  interrupts are taken is observable: taken at once, bof4's CD streaming
  goes another way (plans/unified-adaptor-layer, phase 1 findings).
  So is how long a handler spins, and where it leaves the guest's
  safepoint budget: libpad's adapters spend the quanta its SIO0 and root
  counter waits take in the vblank interrupt (nothing delivered
  meanwhile), then start the budget over as the original's last
  safepoint did (`Regs::reset_budget`, psxw-abi `guest::BUDGET_RESET`;
  D60). libcd's waits (`CD_sync`, `CD_ready`, `CD_cw`) spin as `VSync`'s
  do (D62); inside a callback, where no interrupt is taken, they poll the
  CD controller as the original does.
  Loops that aren't waits count too: libspu's adapters spend the passes
  the translated original's loops make (voice loops, the FIFO's
  halfwords, the copies), reading and setting the guest's budget
  (psxw-abi `guest::BUDGET_GET`/`SET`), with a safepoint where it runs
  out, and call libspu's own delay loop where it does (D63).
- **Blocking runtime calls jump.** A runtime call that must wait (the
  BIOS's `WaitEvent`) advances time event by event until it is satisfied.
- **Asynchronous events.** CD sector delivery, DMA completion, root counter
  ticks and vblank are all scheduled on the virtual timeline. They are
  delivered in a fixed order at safepoints, meaning runtime calls and loop
  back-edges. A device register access doesn't deliver them: an interrupt
  it raises waits for the next safepoint. An adapter that ends a transfer
  the original ended with a register write holds the interrupt the same
  way (`Machine::hold_irq`): it is queued as the adapter returns, for the
  guest's next safepoint, not taken at the adapter's own return (D64).
  And an adapter whose original makes no runtime call before it returns
  doesn't make its return a safepoint (`Machine::quiet`, D63): a
  completion raised in `_spu_Fw` waits past `_spu_FsetRXXa`, the next
  adapter `SpuWritePartly` calls, as it waits past the original's.
- **Wall-clock time only paces presentation.** The same disc, build and
  input log always produce the same frames. That gives replays, golden-frame
  tests in CI, and bisection for free.

**Interrupts are the BIOS's exception handler, run at safepoints** (D45).
An IRQ becomes steps: every `SysEnqIntRP` element, priority 0 to 3, then
the SDK's dispatcher when adapters run it, then the exit. The exit is the
custom one `HookEntryInt` installed if there is one (libetc's, with chip
irq on its device), entered like a longjmp into the `setjmp` that made
its buffer. `ReturnFromException` leaves from any depth.
- **libetc's adapters' dispatcher** (chip irq native, D59) is Rust steps
  in the same queue: for each pending line libetc handles, in line order,
  acknowledge I_STAT and queue the line's handler as a guest call at the
  front, then look again until none is left, as `intr.o`'s loop does.
  libetc's own vblank and DMA handlers are stand-in functions at addresses
  no game code has: libsnd takes line 0 and calls the handler it replaced,
  so a guest call to one runs it in the runtime (`untranslated`), its
  callbacks as nested guest calls. The interrupt controller, DMA
  controller and root counters stay the devices' registers, which the
  adapters change with the devices' own semantics; the emulated chips'
  devices raise their interrupts there as before.
- **Elements the adapters own** (`Sdk::chain_element`, D60): libpad's
  `PadStartCom` queues an element at an address no game code has, and the
  kernel's walk runs it as an adapter step in its place in the chain
  (libpad's vblank poll, when vblank is unmasked and pending).
- **Translated code follows this** through wasm exceptions:
  - a `setjmp` (A0h:13h) caller saves its registers inline;
  - the return address is a continuation entry that reloads them;
  - B0h:17h throws;
  - the guest export `guest_invoke_irq` catches the throw and restores
    `$sp`/`$gp`.
- **So the SDK's own interrupt code runs unchanged.**

Drawing is synchronous inside the runtime: a GPU DMA completes instantly
in virtual time, sets DICR and raises IRQ3 (D42). Its completion callback
still arrives at the next safepoint, through libetc's dispatcher (its
adapters' or its own), not re-entrantly inside `DrawOTag`. libgpu's DrawSync
callback is libgpu's own call: it runs inside the call that drained the
queue, natively as on the device (D61). CD timing is a runtime toggle (D19), with
a per-game default in `game.toml` (`cd_timing`); the CD controller (D58)
has it, whether libcd's adapters drive its core or libcd's own code its
registers (D62):
- *realistic*, because loading screens animate while reads are in flight;
- *instant*, an enhancement.

The game thread runs in a **Web Worker**. The runtime calls the host at
every vblank, in two imports (ABI 10, D54): `host.vblank` carries
everything the guest can see and never suspends; `host.wait_vblank` only
paces, sleeping with `Atomics.wait` until the frame's wall-clock deadline.
Where the browser has JSPI, `wait_vblank` first suspends the game to give
the Worker's event loop a turn: Chromium frees the frames posted to the
page only there, and WebGL context-loss events land there. The guest
can't see any of it.

### 7. One module, with a prebuilt runtime merged in

- **The runtime is prebuilt once per runtime version** (D38). It is
  game-agnostic Rust, built by cargo as a wasm32 `cdylib` that imports the
  guest's memory and keeps its data, stack and heap above the guest window
  (`RUNTIME_BASE`; a debug build's window sits above them, §9). It reads
  and writes guest memory through the window, wherever the guest says it
  is (`guest.guest_window`), and never hands host pointers to the guest.
- **binaryen `wasm-merge` joins it with the linked guest** into one
  `game.wasm`: the guest's `psx.*` imports bind to runtime exports, the
  runtime's `guest.*` imports to guest exports, and `wasm-opt` then inlines
  across the seam. That puts the GTE math in the same module as the game's
  hot transform loops, where cross-module call overhead would hurt most.
- **The runtime is hot-swappable by re-merging**, with no game code
  recompiled or relinked. `psxw-abi` carries an ABI version; the build
  checks the merged module's exports and imports against it, and hosts
  refuse a mismatched `psxw_abi_version`.
- **Dev mode can keep them separate.** Unmerged, the two modules share the
  same memory and the host wires the seam at instantiation (the browser's
  `?dev`). The guest records the adapter table it was linked against, so
  a swapped runtime with another table is refused.
- **The host interface is the only seam.** `game.wasm` imports a small host
  interface: frame pacing, audio output, input, disc reads, and the
  renderer command interface (see Runtime). The browser host implements it
  in the Web Worker. A native **wasmtime** host implements it for tests, CI,
  replays and profiling. Tests therefore run exactly the wasm that ships.
  The browser is the product, not the debugging environment.

### 8. psx-wasm owns the final link

Most of the job is *placement*: data at fixed addresses, function addresses
that are MIPS addresses, overlay stubs, and the guest's half of the seam
with the runtime. wasm-ld can't express any of that.

`psxw-link` reads relocatable wasm objects as described in the
tool-conventions `Linking.md`, using `wasmparser` and `wasm-encoder`. Its
inputs are the game's objects, both decompiled and generated C. It resolves
symbols against the address map and emits the guest module, which is then
merged with the runtime (§7). The guest passes run over guest functions
only, as psxw-link re-encodes each body (`psxw_link::rewrite`):
1. Address masking, including `memory.copy` and `memory.fill`, and the
   I/O-window hook (§1).
2. The `__guest_sp` split.
3. Safepoints.
4. Rewriting indirect calls.

A game that switches threads then gets binaryen's Asyncify, over only the
functions that can be on the stack at a switch (D57). Finally
`wasm-opt -O2` runs on the merged module.

Two alternatives are held in reserve:
- `wasm-ld --emit-relocs` followed by a re-homing pass, if writing the
  linker costs more than expected.
- An LLVM-IR rewriting pass, if masking overhead ever matters. LLVM could
  then hoist and CSE the masks.

### 9. Debug builds: the C in DevTools, variables included (D53)

`psxw build-web <game> --debug` builds a bundle whose `game.wasm` carries
DWARF for every C function, decompiled or lifted. Chrome DevTools (with
the C/C++ DevTools Support extension) or lldb then shows the C, sets
breakpoints on its lines, and reads its variables. Three things make the
debugger's view true:
- **The DWARF follows the guest passes.** clang compiles with `-g`, and
  psxw-link records where each input instruction lands while it rewrites.
  It rewrites every code address, range and line row through that record
  (`psxw_link::dwarf`). Data addresses are where the link put them, and
  the stack pointer is the guest's `$sp`.
- **The window is at `0x8000_0000`.** Every KSEG0 address is then its own
  wasm address. A debugger dereferences the game's pointers, reads its
  globals and finds its stack frames without knowing about the mask.
  Hosts don't commit the 2 GiB below the window, which nothing touches.
  Scratchpad, KUSEG and KSEG1 pointers still read wrong in the debugger.
- **psxw-link merges the guest with the runtime**, since binaryen can't
  carry DWARF through `wasm-merge` (`psxw_link::merge`). The merged code
  isn't optimized further: clang optimized the C, so stepping follows
  `-O2` code.

The DWARF's source paths are relative (`source/game/…`, `source/lift/…`),
and the web bundle carries those files. A debugger resolves the paths
against `game.wasm`'s URL, so DevTools fetches the C from the server,
whatever machine the browser is on.

## Pipeline

```
 inputs.lock ─► psxw inputs sync
                 │  `git archive` snapshots at pinned SHAs (sibling checkouts untouched)
                 │  tracked config: splat configs, symbol files, sources, headers
                 │  original bytes: splat target_path + sha1, from the user's checkout
                 ▼
          psxw-inputs ──► ProjectModel (containers, AddressMap, overlay slots,
                 │                       per-function tier candidates)
                 │ per function: adapter │ decompiled C │ translated
     ┌───────────┴──────────┐            boundary/psyq.toml (library policies)
     ▼                      ▼                + object identity (validated.rs)
 psxw-lift              decompiled C                  │
 original images →           │                        │
 generated C                 │                        │
     └──────────┬───────────┘                         │
                ▼                                     │
            psxw-cc: clang → wasm objects             │
                ▼                                     ▼
            psxw-link         boundary: adapter names from the runtime
                 │  place data/funcs at original addresses, dispatch table,
                 │  o32 adapters, overlay stubs, interior entries
                 ▼
            guest passes (mask + I/O hook, guest SP, safepoints) → guest.wasm
                 ▼  Asyncify, if the game switches threads (D57)
            wasm-merge ◄── prebuilt runtime.wasm (devices, BIOS, adapters)
                 ▼  then wasm-opt

            game.wasm  +  host (browser Worker shell, or wasmtime for tests)
```

## Runtime

The runtime is the PlayStation's operating system and devices,
reimplemented as a Rust library that the compiled game calls into. It never
steps a CPU. The game's own `main` loop is in charge, and game-visible
time moves only inside runtime calls (§6). Callbacks re-enter it: guest
calls runtime, runtime runs a guest callback, and the callback calls the
runtime again. So the runtime never holds a borrow of its own state across
a call into guest code.

- **Boot.** Does the BIOS's startup job: reads `SYSTEM.CNF` from the disc,
  loads the PS-X EXE into RAM, zeroes BSS, sets `__guest_sp`, and calls
  `guest_main`.
- **Threads (D57).** The BIOS keeps the kernel's thread tables in guest
  RAM (the table of tables' PCB and TCB entries, the PCB, and the TCBs
  from E000h, sized by SYSTEM.CNF `TCB=`), because games read and edit
  them. ChangeThread switches wasm stacks with binaryen Asyncify: the
  build instruments only the functions that can be on the stack at a
  switch (the call graph's reverse closure from ChangeThread calls, plus
  every indirect caller), the running thread unwinds into its buffer back
  to its root call in the runtime, and the target starts afresh at its
  TCB's start address or rewinds to its own ChangeThread. A game that
  never switches gets stubs and no instrumentation.
- **BIOS.** The A0/B0/C0 call tables and `syscall`: events (`OpenEvent`,
  `EnableEvent`, `TestEvent`, `WaitEvent`, `DeliverEvent`), the callback
  registry, critical sections, the heap, the file API on top of the CD,
  the memory card functions (D43), exception hooks, and `setjmp` support
  (guest `setjmp`/`longjmp` use wasm exception handling, D45). The pure
  parts (heap, the string table) could come from OpenBIOS's C, compiled as
  guest code.
- **clock.** The virtual timeline: vblank (NTSC 59.94 Hz, PAL 50 Hz), a
  queue of pending device events, and the safepoint quantum. It delivers
  events in a fixed order by calling guest callbacks.
- **The bus** (`psx-bios::io`). Device libraries' I/O-window accesses land
  here (§1). It holds the interrupt controller (`psx-devices::irq`), the
  root counters (`timers`, computed from virtual time rather than ticked),
  the DMA controller (`dma`: transfers complete instantly in virtual time,
  then set DICR and raise IRQ3), and the devices below. A raised interrupt
  queues the BIOS exception handler's steps (§6).
- **gpu.** Split in two at a renderer command interface, so the PS1
  semantics stay host-agnostic:
  - **Front end, in the runtime** (`psx-devices::gpu`). Takes GP0 words
    from the port or DMA2 and GP1 commands, and answers GPUSTAT and
    GPUREAD (D42). An ordering table is one linked-list DMA, walked in
    Rust. It decodes packets, and tracks draw/display environments,
    texture pages and windows. It batches primitives and tracks VRAM
    hazards. It emits renderer commands (draw batch, VRAM upload, VRAM
    copy, VRAM readback, present), which reach the host at each vblank
    and at every VRAM→CPU transfer.
  - **WebGL2 backend, in the host.** Rust over `glow`, which targets WebGL2
    in the browser and native GLES for the test host. This is a
    GPU-accelerated renderer from the start, built the way emulators build
    theirs:
    - **VRAM is one 1024×512 `R16UI` texture** holding the exact 16-bit
      pixel values. Primitives render into it, and the fragment shader
      samples textures from it, decoding 4/8-bit texels through their colour
      lookup tables itself. It has to be, because games render into VRAM and
      then texture from the result.
    - **All pixel maths is integer shader code,** so 1× output can be
      byte-exact against the oracle (§Oracles):
      - Each primitive draws a covering quad. The fragment shader evaluates
        the PS1's edge functions, fill rules, and Gouraud and texture
        interpolation in integers.
      - Semi-transparency and the mask-bit check need the destination
        pixel. WebGL2 has no framebuffer fetch and can't blend into integer
        textures, so the shader reads the destination from a read copy of
        VRAM instead of using fixed-function blending.
      - Batches split where a primitive overlaps an earlier write it must
        read. The written region is copied into the read copy first.
    - **Feedback hazards.** A primitive that samples the area it draws into
      reads from the read copy, which holds the pre-draw contents.
    - **The rest of the pixel pipeline is shader work too:** colour
      modulation, the 4×4 dither matrix, texture windows.
    - **CPU transfers.** `StoreImage` (8 call sites in btn) is a
      synchronous `readPixels`, which is fine in the worker. `MoveImage` (13
      sites) is a GPU-side copy.
    - **Exactness.** It starts at 1× resolution, with upscaling later. The
      remaining inexactness is 1-pixel rasterization differences on edges,
      which only presentation sees.
- **gte.** `psx-gte`: exact fixed-point math. Its register file lives in the
  runtime-private page of the guest window, so C macros, translated COP2
  code and adapters all see the same state.
- **spu.** `psx-spu`, at 1F801C00h with DMA4 and IRQ9 (D47): 512 KB of
  SPU RAM, 24 voices with ADPCM, ADSR, pitch and noise, plus reverb and
  CD-input mixing. It runs on its own 44.1 kHz clock, brought up to the
  current cycle before every register access, every DMA4 transfer and
  every vblank. Its mix is what `host.audio` carries.
- **cd.** Disc images (`.bin`/`.cue`, `.iso`), sectors verified by EDC, and
  a drive model: sector reads scheduled on the clock (with the timing
  toggle from §6), and XA-ADPCM streaming into the SPU's CD input. The CD
  controller (`psx-devices::cdc`, 1F801800h-3h, IRQ2, DMA3, D58) answers
  commands with INT3/INT2 after the timing model's latencies and delivers
  sectors through its data FIFO, XA through the decoder. Original libcd
  code drives its registers; libcd's adapters (D62) drive its core
  (`Cdc::issue`, `pending`, `acknowledge`, `request` …), the same
  controller without the register protocol.
- **mdec.** The motion decoder (1F801820h/24h, DMA0 in, DMA1 out) for
  libpress's FMV: run-length decode, IDCT and colour conversion, instant
  in virtual time (D58). Its decode function (`Mdec::input`, `output`)
  is what DMA0 and DMA1 call, and what libpress's adapters call without
  the register protocol (D64).
- **sio.** SIO0 at register level (1F801040h-1F80104Eh, IRQ7), a
  bit-level port of the oracle's FrontIO and digital pad, so libpad's own
  state machine sees the timing it sees on the oracle (D40): chip sio0's
  fallback, and the oracle for libpad's adapters (D60). Memory cards
  are raw 128 KiB `.mcr` images in host storage, read and written through
  the BIOS card functions (D43).
- **SDK adapters.** The thin, object-keyed layer from §5.

### Host scripting

`psxw_execute(id, addr, a0..a3)` queues one call into the guest (D52). The
request slot is separate from the machine: the browser can fill it while
JSPI has suspended `wait_vblank`, or from `host.vblank`, even though those
stacks hold a mutable machine borrow. Once the driver has released the
borrow and drained interrupts and callbacks, it executes the request on
the normal guest stack, preserving `$sp`/`$gp`. Completion is an `execute`
event through `host.report`, with the request id and the raw o32 result as
a decimal string. Calls cannot nest; a trapping call stops the run.

The page's `psx.execute(address, ...args)` is a
Promise-based bridge to that slot; `cmd(name, ...args)` and `load(url)`
reach script mods. Commands and `psx.run` callbacks of script mods run
through the same slot, at synthetic addresses. The build's symbol
addresses, and the snapshots below, are the references; source maps are
not part of the contract.

### Script mods (D54, D55)

Script mods are the modkit; compiled mods (M11) are deprecated but keep
working (D56). A script mod is one ES module whose default export, `mod(factory)`, given
the mod's `game` and the mod API, returns its hooks (`before`, `after`,
`replace`, `around`), data patches and commands. It loads into a moddable
base (D51) beside compiled mods, with no build step (`web/script.js`).

- **A shim makes it a layer.** The loader writes a small wasm module for
  each script mod (`web/shims.js`): `hook.<k>` for each slot it hooks, in
  the slot's own type, which joins the slot's chain in load order like a
  compiled layer; `next.<k>` for `around`'s `original`; o32 entries for its
  commands and `fnptr` values at synthetic addresses. i64 values cross as
  two i32 words, so no BigInt crosses per call.
- **The runtime holds the host's vblank wait while JS is on the stack**
  (ABI 10). JSPI can't suspend through a JS frame, and a vblank can be
  delivered at any safepoint in any game code a hook calls. So the host's
  vblank is two imports:
  - `host.vblank(n)` never suspends, and carries everything the guest can
    see (the frame counter replay input is keyed on, swaps, queued calls);
  - `host.wait_vblank(n)` only paces and yields.

  Shims bracket each call into JS with `psxw_js_enter`/`psxw_js_leave`.
  While any is open the runtime holds `wait_vblank` calls, and makes them
  in order once the outermost returns (`psx-runtime` `hold`). The guest
  can't see the hold; only the page's pacing slips. The run report counts
  held waits.
- **Game calls happen inside the game:** in hooks, commands, `init` and
  `psx.run`, never from the host's vblank. Memory reads and writes work
  anywhere.
- **`around` is exact by default.** Calling `original` with the arguments
  it was given passes the original words on (an o32 call's pad words, a
  value typed narrower than its register), and returning `undefined`
  passes the original's result on whole, `v1` included. With both, a
  pass-through is bit-identical to the plain build.
- **Defaults taken in M12.** `after` gets the result first, `undefined`
  for a `void` target. Reloading a script mod runs its factory again, so
  its state resets. A handler that throws is logged, the call passes to
  the next link, and its mod is disabled at the next vblank. Mods are
  JavaScript with JSDoc, checked against the generated `.d.ts` by the
  editor and by `tsc`; there is no compile step. Script mods are trusted
  code, as compiled mods are: no sandbox.
- **Names and types come from snapshots** keyed by the game binary (D55):
  neutral JSON, one source's view of the binary, produced by frontends
  (symbol lists, C headers with the layout probe, DWARF). `game` and the
  editor's `game.d.ts` are generated from snapshots, never from a source
  directly.
- **Tests run in the browsers.** The native host runs no JS; a script
  mod's tests run in headless Chromium and Firefox and report in the
  native host's shapes, so M11's expectations read them alike.
- **Debug builds load them.** A debug build's moddable base (D53's
  window at 2 GiB) loads script mods, which read the window from the
  module: the game's C in DevTools with mods applied.

## Browser host

- **Worker.** Runs `game.wasm` and the host interface it imports, and owns
  the clock.
- **Presentation.** OffscreenCanvas inside the worker, drawing with the
  WebGL2 backend.
- **Audio.** At each vblank the runtime hands the host the 44.1 kHz frames
  that fell due in virtual time (`host.audio`). The worker writes them
  into a SharedArrayBuffer ring, and an AudioWorklet drains it. Pacing
  still waits for each frame's wall-clock deadline. Letting the ring's
  fill level correct it, which would make audio the master clock, is not
  done yet.
- **Input.** The main thread polls the Gamepad API (it isn't available in
  workers) and keyboard state, and writes them into a SharedArrayBuffer.
- **Disc.** The user drops a `.bin`/`.cue` file, which is copied into OPFS.
  The worker reads it synchronously through `FileSystemSyncAccessHandle`.
  Memory cards are stored in OPFS too, as raw `.mcr` images the page
  imports and exports.
- **Context loss.** VRAM is shadowed every 60 frames and uploaded into a
  new renderer when the WebGL context comes back.
- **Cross-origin isolation.** `SharedArrayBuffer` requires COOP/COEP
  headers. `psxw serve` sets them; a static host needs them configured.
  A webview shell (Tauri/wry) could set them on its custom protocol, but
  for now the target is a browser tab only (D46).

## Oracles

**The rule:** every component needs an automated oracle that we didn't
write. A component without one isn't done. If no oracle is available yet,
the component is *provisional*: it can ship so the game runs, but every
report marks it unverified and nothing calls it correct.

**There are four kinds of evidence, and only the first three are oracles:**

| Kind | What it is | Status |
|---|---|---|
| **The original code** | The matching decomp and the original objects define the behaviour of the game and every SDK library completely | True oracle |
| **Exact by construction** | Results fully determined by the data or by checksums: VRAM fills, copies and transfers; 1:1 unblended sprites and colour-table decoding; CD sector error-detection codes; redump.org disc hashes; the decomp's recorded file hashes; memory card frame checksums; the game's own save checksums | True oracle |
| **A device target** | Vectors run on a real PS1 (*hardware*), or on a headless emulator standing in for it | Hardware is the true oracle; an emulator is a provisional one |
| **Models** | Our own reference code, specs transcribed by us | Not an oracle; debugging aid only |

### Oracles from the original code

- **Translated vs decompiled C**, for every function that has both:
  thousands of functions across the two repos. This is the idea in
  btn-decompilation `plans/static-recompilation-project`.
- **Adapters vs the original library over the devices** (`psxw conform`,
  §5). One scenario of SDK calls and vblanks runs on the adapters and on
  the translated original library over the register-level devices, and
  what the game can observe after each step must agree. A device library
  is exact by construction wherever its device is: its own code decides
  every value, for any SDK version.
- **Build-time invariants:**
  - each guest address has exactly one implementation;
  - C-defined data equals the original bytes;
  - struct layouts agree between GCC and clang (the M0 probe);
  - every cross-tier call agrees on its prototype;
  - resident-code fingerprints match the user's disc;
  - after boot, RAM equals the EXE image.
- **The C tier under clang.** Matched C is tuned to GCC 2.95 and may rely
  on undefined behaviour: signed overflow, uninitialized locals used to
  steer register allocation, type punning, out-of-bounds indexing into
  neighbouring fields. Mitigations:
  - compile with `-std=gnu89 -fwrapv -fno-strict-aliasing
    -fno-delete-null-pointer-checks -funsigned-char -ffreestanding
    -ftrivial-auto-var-init=zero`;
  - test builds add `-fsanitize=undefined`, with trap handlers the runtime
    implements;
  - translated-vs-C tests catch whatever the flags miss.

### The device oracle harness

Device behaviour has one source of truth: the hardware. The harness makes
that an automated oracle. The device target is pluggable: headless
emulators now, a real console when one is available.

```
 psx-wasm run of the game ──► vector recorder ──► vector archive
                                                    │
                ┌───────────────────────────────────┤
                ▼                                   ▼
   runner.exe on a device target          psx-wasm device under test
   (emulator A, emulator B, or a PS1)     (same vector)
                │                                   │
                ▼                                   │
   golden store (result + provenance) ──► compare ◄─┘
```

- **The vector recorder** captures, from real psx-wasm runs, what the game
  asks of each device: draw commands with the VRAM state before them, GTE
  inputs, SPU voice setups and sample counts, CD command sequences. Coverage
  is the game's real workload.
- **The runner** is one small PS1 program, built with PSn00bSDK, that
  executes vectors directly on the chips:
  - load VRAM, send draw commands, read VRAM back;
  - load GTE registers, run an operation, store the results and FLAG;
  - set up SPU voices, run N samples, read the SPU RAM capture buffers,
    the reverb work area and the voice status registers;
  - issue CD commands and record status and arrival times;
  - feed the MDEC tables and macroblocks and read the decoded pixels;
  - call the BIOS's thread functions and read the TCBs back.

  It writes results to main RAM. A target therefore only needs to load an
  EXE, run headless, and dump main RAM. On a console, vectors and results
  travel over the serial port instead.
- **Targets:**
  - *An emulator now:* Beetle PSX (Mednafen's PlayStation core), driven by
    a small headless libretro host. It is built from source at a pinned
    commit inside a Docker image, with OpenBIOS (also built from source) as
    the BIOS, so no BIOS dump is needed. It runs as an external tool and is
    never linked into psx-wasm, so its license doesn't touch it.
  - *Hardware later:* an original PS1 with a serial port, a USB-serial
    adapter, and FreePSXBoot or Unirom to run the runner. Anyone with such
    a rig can run a batch of vectors, since the harness is data in, data
    out.
- **Every golden result records its provenance:** "hardware" or "emulator
  X at commit Y". Verdicts against emulator results are provisional.
- **One emulator.** The stand-in is Mednafen's PlayStation core (Beetle
  PSX). It's the most accurate available, though no PS1 emulator is fully
  cycle-accurate. Where it is known to be weak (CD drive timing), vectors
  stay *unresolved* until hardware. Its known inaccuracies are recorded as
  limits of the oracle.
- **The upgrade path.** When hardware is available, the whole vector
  archive runs on it once. Every verdict is recomputed, and every place an
  emulator disagrees with hardware becomes a documented finding.

### Oracle status per device

| Device | Oracle | Strength today |
|---|---|---|
| GTE | Device target (exact results and FLAG) | Provisional, strong: Mednafen matches hardware test results bit for bit |
| GPU: fills, transfers, copies, 1:1 sprites, colour tables | Exact by construction | Verified |
| GPU: polygons, lines, blending, dithering, mask bit | Device target (VRAM readback, byte for byte at 1×) | Provisional, strong |
| SPU: voices, envelopes, interpolation, reverb | Device target (SPU RAM capture buffers, reverb work area, status registers) | Provisional, good |
| XA decoding | Device target (decoded CD audio lands in the SPU RAM capture buffers) | Provisional, good |
| CD data | Exact by construction (sector codes, disc and file hashes) | Verified |
| CD controller: commands, responses, interrupt types, sector data | Device target (`cd_cmds`); sector data also by EDC | Provisional. Times and the head position are recorded, not compared (D48) |
| CD timing | Device target: measured envelope; ours must fall inside it | Hardware only. Emulators approximate drive timing |
| MDEC | Device target (`mdec_run`: decoded output at every depth) | Provisional |
| Memory card | Exact by construction (format checksums; the game accepts its own saves) | Verified |
| Pad | Exact report formats; the original libpad over SIO0, a port of the oracle's (D40); libpad's adapters against it (D60) | Verified |
| BIOS | Device target running BIOS calls (`bios_threads` for threads); a legitimately dumped BIOS image can also be translated and run as the oracle | Provisional |

Byte-exact GPU verdicts at 1× require exact rasterization in the WebGL2
backend: each primitive draws a covering quad, and the fragment shader runs
the PS1's edge and interpolation maths in integers. Upscaling and other
enhancements can't be checked against 1× hardware. They are layered on the
verified 1× mode as explicitly unverified options.

### Whole-game oracles

- **RAM by symbol against an emulator** (a diagnostic). The original game
  runs in a headless emulator with the same inputs as psx-wasm. Because the
  memory layout is the original's, RAM can be compared by symbol at
  matching values of the game's own frame counter: "at frame 4,210,
  `gPlayerX` differs". `psxw oracle play` runs the emulator side and
  `psxw oracle ramdiff` the comparison (`--compare-frames` adds VRAM and
  frames); conventions in D39. It reports and doesn't gate while CD timing
  is unresolved: load times differ between drive models, and games step
  their RNG while loading (D48).
- **What the game can observe must be right.** VRAM readback, SPU key
  status, CD status, pad data and timer values feed back into game logic.
  Pixels on screen and audio samples are presentation. Timing has a range,
  not a value: CD seek times varied on real hardware and games were written
  to tolerate that, so realistic timing must fall inside the hardware's
  measured envelope.
- **The game checks itself.**
  - Deterministic replays (same disc, build, pads plugged in and input
    log) give golden frame and audio hashes for regressions.
  - A replay can declare reached-state checks, `SYMBOL==VALUE@COUNTER`:
    RAM predicates by symbol that must hold once the frame counter
    reaches each value (M13).
  - RAM invariants: no access into the I/O range outside device libraries
    (`no-shadow-io`), and the stack within bounds.
  - The game's own integrity checks, such as save checksums, pass.

## Pinning the inputs

Both decompilations are moving targets, so this repo pins exactly what it
consumes:
- **`inputs.lock`** records each input repo's commit SHA, the pinned
  toolchain (clang, binaryen and the rest, each by URL and sha256), the
  oracle images' pins, and the expected hashes of the user-supplied disc
  files. The disc files themselves are never committed.
- **`psxw inputs sync`** snapshots each repo at its pinned SHA with
  `git archive` into `.inputs/<name>-<sha>/`. The local checkout is used
  only as an object store: no worktree metadata is added to it, and its
  uncommitted work is never read (btn-decompilation had 147 modified paths
  on 2026-10-25).
- **Original bytes** come from the user's checkout at each splat config's
  `target_path`, accepted only if they match the config's `sha1`. Any
  splat-based decomp already declares both, so no repo-specific manifest is
  needed.
- **A pin moves by editing `inputs.lock`**, and `psxw report tiers`
  before and after shows what changed tier. A `psxw inputs bump` that
  prints the change ("37 functions moved lift → c") isn't built.
- **Matching builds only.** psxw never builds a decomp repo (D25); the
  original bytes it accepts must match the splat `sha1`, and a decomp is
  pinned at a commit whose own checksum gate (`make check-all`,
  `npm run check-all`) passes.

## Roadmap

The milestones, one file each, are kept as history in
[`plans/completed/`](../plans/completed/); the plan index, the active plan
and the open items are in [`plans/README.md`](../plans/README.md). Each
milestone's exit is gated by an automated oracle.

M0–M8 and M11–M13 are implemented, and M10's parts that need no rig.
Some of their tasks stay open: the
whole-game RAM equality gates are blocked on CD timing (D48), some wait on
the human (recordings, a hardware rig for M10, pushing the harness), and
M8 left the translated-SDK cache and the per-function psyq-decomp
integration (its tasks 5–6). M9
never started; M13 brought up a second game, bof4, which needed
toolchain work. The active plan is the unified adapter layer.

| # | Milestone | Gating oracle |
|---|---|---|
| M0 | Census | `psxw census` passes (see below) |
| M1 | Translator and linker core | `psxw lift` and `psxw difftest` pass (see below) |
| M2 | Whole-game link and headless boot | RAM equals the EXE image after boot; CD reads verify; overlays dispatch; main loop reached; deterministic |
| M3 | Oracle harness (Docker image: Beetle PSX + OpenBIOS; PS1 runner; vector recorder; golden store) | Exact-by-construction vectors reproduce through the emulator; deterministic captures |
| M4 | Pixels: GTE, GPU front end, WebGL2 backend (exact at 1×), pad; libgpu, libetc and libpad became device libraries (D40, D42, D45) | GTE and GPU vectors byte-exact against goldens; conformance of libgpu, libetc and libpad; RAM by symbol through a replay (a diagnostic, D48) |
| M5 | Browser host | Identical frame and RAM hashes to the native host for the same replay |
| M6 | Sound: SPU, XA; libspu and libsnd over the register-level SPU (D47) | SPU and XA vectors against goldens; libspu and libsnd conformance; RAM including sound-driver symbols (a diagnostic, D48) |
| M7 | Saves and a full play session | Card checksums; the game accepts its own saves; RAM by symbol over an in-game day (a diagnostic, D48) |
| M8 | SDK generality: object-keyed adapters, layout generation, translated SDK cache, psyq-decomp C | Adapter conformance per object; psyq-decomp C matches translated |
| M9 | Second game | The same oracles pass with only a manifest |
| M10 | Hardware oracle (when a rig exists) | The vector archive replayed on a real console |
| M11 | Modkit: declared hooks on function boundaries; mods linked in, then loaded and hot-swapped as wasm | Example mods' effects by replay; an empty mod set builds the plain game; static and dynamic backends agree |
| M12 | Script mods: JavaScript hooks and commands on the moddable base, live in the browser; types from snapshots keyed by the game binary | Script twins equal their compiled mods bit for bit; a script pass-through on every hookable function equals the plain build |
| M13 | Breath of Fire IV, a game with no decompiled C: threads (D57), the CD controller and MDEC (D58), kernel tables in guest RAM | `psxw build bof4` with no unbound hardware; boot, determinism, no-trap and no-shadow-io; the replay's reached-state checks; CD controller, MDEC and thread vectors |

### M0 result: btn @ 3a15992 (2026-10-25)

`psxw census btn` passes, in about 14 seconds:

- **Compile.** All 1,802 TUs compile with clang 23.1.0 (wasi-sdk-34).
- **Functions.** Every function is accounted for. The per-container totals
  (exe 462, ovl_11 1,606, ovl_15 135, …) equal the decomp's own function
  counts.
  - 2,584 functions build as C.
  - 90 go to the lift tier: 49 `INCLUDE_ASM`, 21 with real inline asm, and
    20 GNU nested functions (outer and inner).
- **References.** All 10,137 references resolve. 9 of them go to host
  support functions: `abs`, which GCC inlined.
- **Layout.** All 8,374 layout entries match across 253 headers. 8 headers
  were skipped: 3 are assembler-only and 5 are included by no TU. A
  negative test confirms the probe catches a real difference: `long double`
  is 8 bytes on o32 and 16 on wasm32.
- **C-defined data.** This game defines no initialized named data in C.
  splat placed its data in asm, so data comes from the original image at
  boot and the layout probe carries the struct-layout check.
- **GCC 2.95 normalizations clang needed:**
  - incomplete-element arrays, moved or dropped (all TUs);
  - register-pinning asm (47);
  - one file-scope global register variable;
  - `break` asm becoming a runtime call;
  - asm barriers, kept.
- **Portability signals for the linker.** clang's warnings point at C
  that's well-defined on o32 but risky on wasm:
  - 138 `-Wint-conversion` and 97 `-Wincompatible-pointer-types`;
  - 32 signature-mismatch thunks (`FntPrint`, `KanjiFntPrint` called
    through K&R declarations), which need o32 adapters;
  - 6 interior entry points inside sized functions, which need entry stubs
    if anything calls them.

### M1 result: btn @ 3a15992 (2026-10-26)

- **Lift.** `psxw lift btn --all` translates and compiles all 3,359
  functions (164,741 instructions) in about 9 seconds:
  - 178 jump tables;
  - 3 indirect tail calls, all in SDK objects;
  - 208 indirect calls;
  - 63 BIOS calls.

  I/O and BIOS-kernel accesses all sit in SDK objects then destined for
  adapters (`libpad/pdresres`, `libmcx/*`, `libcard/patch`,
  `libapi/first`); libpad has since run as original code over SIO0 (D40).
- **Inline expansion.** 13 functions inline callees that pass values in
  registers: the libgte `msc02` cluster, the BIOS GTE patch stub, and 7
  undecompiled game functions that return their caller's `v0`.
- **Differential harness.** `psxw difftest btn --seeds 64` takes about 6
  seconds. Each pair of decompiled C (compiled as if alone) and translated
  original runs from the same memory with ABI-shaped seeded inputs. Every
  call they make is logged and answered identically.
  - **Result:** 2,567 pass and 3 are excluded with recorded evidence.
  - **The exclusions:** local-array UB reachable only with out-of-contract
    inputs (`games/btn/game.toml [difftest.exclude]`).
- **What triage turned into rules.** Each mismatch class became a general
  rule or a fixed bug, not a pin:
  - **Lift-tier rules:** global register variables, register-capture
    locals, falling off a non-void function, and soft-float arithmetic.
    That's 18 more functions in the lift tier, 2,566 C in all.
  - **Link conventions:** GCC's division checks, builtin `memcpy` and
    `__main`.
  - **Bugs fixed:** auto-names restricted to C identifiers (clang's
    `.Lswitch.table.func_X` had resolved to the function's own address);
    jump-table indices keep their identity through constants.

## Risks

| Risk | Mitigation |
|---|---|
| clang miscompiles matched C that relies on UB | Containment flags, UBSan in test builds, differential tests of C vs lifted. A function can be pinned to `lift` in the game manifest. |
| A custom linker is real work | The wasm object format is small and stable, and the M0 census exercises it early. The `--emit-relocs` re-homing approach is the fallback. |
| A struct layout differs between GCC o32 and clang wasm32 | Caught at M0 by the layout probe (both compilers on every header the game includes). None found for btn. Per-type fixes would go in the prelude. |
| Game logic depends on CPU timing (root-counter measurements, busy-wait calibration) | The safepoint budget (set at link time) and quantum (a boot option) can be tuned. Such sites can be pinned to replaced or adjusted implementations. |
| Handwritten SDK asm uses non-ABI register conventions | Inline expansion: a callee that passes values in registers is expanded into its callers, every register shared (D33). |
| PSX RAM mirroring (`0x80200000` aliasing `0x80000000`) is relied on | The aux region occupies the mirrors. If a game needs mirroring, it can be enabled by masking with `0x1FFFFF` for that game. |
| Browser constraints: COOP/COEP, Gamepad API only on the main thread, Safari gaps | A Worker paced with `Atomics.wait` over SharedArrayBuffers, yielding through JSPI where the browser has it; a browser tab only, for now (D46). |
| WebGL2 limits: no blending into integer textures, no framebuffer fetch | `R16UI` VRAM with all blending and mask logic in integer shader code; the destination is read from a read copy of VRAM; batches split on overlap. |
| An SDK object with no adapter (new version, patched library) | Translate it and cut one level lower, down to the BIOS calls. Failing that, the build names the functions whose objects need adapters, or their library a `device` policy, so the original runs over the register-level devices. |

## Open questions

1. **Where the shipped boundary sits.** D40–D58 put it at the hardware
   registers, so both games run on register-level device models. Whether
   to move it back up is the human's open decision; the unified adapter
   layer plan (`plans/unified-adaptor-layer/`, being written) proposes
   native adapters per chip with the device models as the fallback.
2. **Distribution posture.** `game.wasm` contains compiled game code but
   no disc data. Will it be distributed, or only built locally from the
   user's own checkouts?
3. **Q3. The license for psx-wasm.** Needed before any public release.
   Constraint so far: `psx-devices::sio` is a close Rust port of Beetle
   PSX's `frontio.c` (GPLv2), made so SIO0 matches the oracle bit for bit.
   A license that isn't GPL-compatible means re-implementing it from a
   behavioural description. Everything else is written from psx-spx, with
   the emulator used only as an oracle.

Decided or settled:
- **Q1. Webview shell (Tauri) or browser tab only.** Browser tab only, for
  now (D46).
- **Q2. Register-level SPU fallback for unknown libspu versions.** Yes
  (D41); since D47 it is the way for every version.
- **Q4. The second game.** To be decided in M9, which never started. M13
  brought up bof4.
- **Export bundles** from the decompilation harness (ELFs, objects,
  `compile_commands.json`). Not needed: the translator reads the original
  images (D10), and psxw never builds a decomp repo (D25).

## Decision log

Settled decisions, with their reasons, numbered in the order they were
made. Code comments cite them by number (D45). A later decision
supersedes an earlier one where they conflict, and the body above states
what holds now; a note marks entries that later decisions changed. Each
entry points to the section that elaborates (`design §N` is a section of
this document). The open questions Q1–Q4 are under Open questions above.

To change a decision, record the proposal and evidence in the active
plan's Findings and ask the human. Don't silently diverge.

### Product and scope

**D1. What psx-wasm is.** A toolchain that turns a matching PlayStation
decompilation into a native WebAssembly build. It does no CPU emulation
and ships no register-level hardware model. It is a source port plus a
compatibility layer: high-level reproduction of the console's OS and
device behaviour, in the same family as libultraship, PsyCross and Wine.
*Why:* the human wants a truly native result, not an emulator.
(design: Goal; What "native, not an emulator" means)

*Later:* D40–D58 moved the boundary below the SDK, so the hardware there
is modelled at register level. There is still no CPU emulation.

**D2. psx-wasm vs a static recompiler.** Static recompilers translate all
machine code and emulate the hardware around it. psx-wasm compiles the
source and translates only the gaps, so as decomp coverage rises it
becomes a source port. Its value is a moddable, enhanceable port and a
general toolchain, not merely playability.

**D3. Game-agnostic.** Harvest Moon: Back to Nature (btn) is the first
target, not the design centre. Game facts live only in
`games/<id>/game.toml`, SDK facts in `boundary/`.

**D4. Not built.**
- a MIPS interpreter or dynamic recompiler;
- register-level GPU, CD or pad emulation;
- a BIOS ROM.

Open: register-level SPU as a fallback for unknown libspu versions (M8).

Later decisions made the pad (D40), GPU (D42), SPU (D41, D47), CD
controller and MDEC (D58) register-level devices, for SDK code that
drives them itself. There is still no CPU emulation.

### Memory and calls

**D5. Keep the PSX address space.** Every symbol sits at its original
address in a 16 MiB window at `0x0100_0000`. Effective addresses are
`(addr & 0x00FF_FFFF) + base`, and pointers keep their PSX values. *Why:*
no source patches, pointers in assets keep working, data is verifiable,
and RAM is comparable by symbol. The M0 layout probe validated it (8,374
of 8,374 entries match). (design §1)

**D6. Function pointers are MIPS addresses.** One dispatch table,
`table_index = (addr & 0x003F_FFFC) >> 2`. Indirect calls use the o32
indirect ABI `(a0..a3) -> i64`. (design §2)

**D7. Overlays dispatch on resident bytes.** A stub compares code words in
RAM against each candidate overlay. *Why:* it's how hardware decides, and
needs no per-game loader hook. (design §3)

### Implementations

**D8. One implementation per function.** In priority order:
1. an *adapter*, if the function touches hardware registers or BIOS
   internals;
2. else *decompiled C*;
3. else *translated*.

The rule is applied per function or member, never per library.
(design §4, §5)

*Later:* whether a function may have an adapter is set per library by
its policy in `boundary/psyq.toml`. Device libraries get none (D40), and
`adapter-or-device` decides a whole library at once (D58).

**D9. Translate into generated C.** It is compiled by the same clang
pipeline, not emitted as wasm, Rust or LLVM IR. *Why:* one pipeline, clang
optimizes it, it's debuggable through `#line`, and N64Recomp and
PSXRecomp prove the approach. (design §4)

**D10. The translator reads original images.** Per game, it reads the
original EXE and overlay images, which is valid because of D5. Relocatable
SDK members are used only for the reusable translated-SDK cache. *Why:* no
dependency on building the decomp repo, and no export bundle needed.
(design §4)

**D11. Generated functions use the o32 form `u64 f(a0..a3)`.** The linker
puts adapters at every edge where C and generated code call each other,
derived from the C side's wasm signature plus prototype facts (structs
passed by value, variadics). *Why:* no dependence on prototypes for asm
functions, and functions can change tier freely. (design §4)

**D12. The SDK is split into three runtime layers:**
- stable, hardware-defined **devices**;
- the **BIOS**, identical for every SDK version;
- thin **adapters**, keyed by object identity (hash), with struct layouts
  generated from each SDK version's headers.

(design §5)

*Later:* the devices are register-level models on the I/O bus, which
device libraries drive directly (D40, D42, D45, D47, D58).

**D13. libapi is translated.** 33 of the 39 members btn links are BIOS
trampolines, which the translator routes to the Rust BIOS. Adapters are
needed only for `counter.o` (it programs the timers) and for the members
that patch the Sony BIOS (`pad.o`, `patch.o`, `chclrpad.o`). (design §5)

*Later:* the adapters are for `counter.o`, `pad.o`, `patch.o` and
`first.o` (`firstfile`). `chclrpad.o` touches no hardware and runs as
guest code (M13 task 15).

**D14. Pure libraries run as guest code.** libsnd, libgs, libgte, libmath
and libc are never replaced for correctness; an adapter is allowed only
for speed, where equivalence is easy to show. *Why:* their behaviour is
what the game shows, plays and decides (`rand`, the music sequencer).
(design §5)

*Later:* libsnd stays original code but is a device library, because its
own SPU register writes must reach the SPU (D47).

**D15. The decompiled SDK is not on the critical path.** Translation
covers everything. Decomp effort pays off most on the boundary libraries,
where it specifies the adapters.

### Runtime and host

**D16. The runtime is a precompiled Rust archive linked into
`game.wasm`.** Swapping it means a relink. Dev mode keeps it a separate
module sharing memory. The host interface is the only seam, and tests run
the exact wasm that ships. (design §7)

*Later:* superseded by D38: the runtime is merged, not linked.

**D17. psx-wasm owns the final link.** wasm-ld can't express placement.
`wasm-ld --emit-relocs` plus a re-homing pass is the fallback. (design §8)

**D18. Time is virtual and deterministic.** It moves at blocking calls and
safepoints, and callbacks run at safepoints. The browser runs it in a Web
Worker paced with `Atomics.wait`; JSPI is the fallback. (design §6)

*Later:* libetc's `VSync` and libgpu's `DrawSync` run as original code
(D42, D45) and wait by polling, so time mostly moves at safepoints. Where
the browser has JSPI, the Worker uses it besides `Atomics.wait`, to yield
to its event loop at each vblank.

**D19. CD timing is a runtime toggle** (realistic or instant), with a
per-game default. (design §6)

**D20. The GPU uses WebGL2, not WebGPU.** A front end runs in the runtime;
the backend runs in the host, in Rust over `glow`, which gives WebGL2 in
the browser and GLES natively.
- VRAM is an `R16UI` texture.
- All pixel maths is integer shader code: rasterization, interpolation,
  blending, mask and dither.
- The destination is read from a read copy of VRAM, and batches split
  where primitives overlap.
- The goal is byte-exact output at 1×. Upscaling and other enhancements
  layer on top later, as unverified options.

This supersedes "software rasterizer first" and "RGBA8 + stencil".
(design: Runtime)

### Verification

**D21. The oracle rule.** Every component needs an automated oracle we
didn't write. A component without one is *provisional* (shippable,
labelled unverified), never "correct". (design: Oracles)

**D22. Oracle kinds:**
- the original code (decompiled C, original libraries);
- results exact by construction (checksums, hashes, fully determined
  results);
- device targets: hardware is a true oracle, an emulator a provisional
  one.

Models (our own reference code, transcribed specs) are debugging aids
only.

**D23. One emulator, dockerized.** Beetle PSX (Mednafen's PS1 core), built
from source at a pinned commit in Docker, with OpenBIOS built from source,
is the device target until hardware exists. No two-emulator consensus.
Known-weak areas (CD drive timing) stay unresolved. *Why:* the human
judged one accurate emulator sufficient, and wants it containerized.
(design: Oracles)

**D24. Hardware later, through the same runner** (M10). Every golden
records its provenance. When hardware arrives, the whole archive reruns.

### Inputs and process

**D25. Inputs are pinned** in `inputs.lock` and read through `git archive`
snapshots. No worktrees, and no builds of the decomp repos.

**D26. Original bytes come from splat** `target_path` + `sha1`, not from
harness-specific manifests.

**D27. Toolchain.**
- clang 23.1.0 from wasi-sdk 34 (freestanding wasm32);
- UB containment flags `-std=gnu89 -fwrapv -fno-strict-aliasing
  -fno-delete-null-pointer-checks -funsigned-char -ffreestanding
  -ftrivial-auto-var-init=zero`;
- Rust 1.94.1.

**D28. GCC 2.95-isms are normalized on preprocessed text**, never by
editing sources. The rules found in M0:
- incomplete-element arrays are moved after their struct, or dropped;
- register-pinning asm is stripped (file scope becomes `static`);
- any `.include "…/NAME.s"` file-scope asm becomes a lift marker;
- `break` becomes `__psxw_break`;
- empty asm barriers are kept;
- functions with real asm or GNU nested functions are lifted, inner
  nested functions included.

M1 added four lift rules, each for C that can't say what the original does
(see D35).

**D29. Host support functions.** GCC inlined some builtins (`abs`,
`labs`, `fabs`), so the original binaries carry no copy; the runtime
supplies them.

**D30. Milestone order.**
- Translator before boot.
- The oracle harness before devices (it can run in parallel with M1–M2).
- Browser before sound, though that pair is swappable.

**D31. Commits are allowed but carry no Claude attribution.** Nothing
game-derived is ever committed.

**D32. No filesystem searching.** Inputs outside psx-wasm and the named
decomp repos come from paths the human supplies.

### M1: translator, harness and toolchain

**D33. Register passing is handled by inline expansion, not full-state
mode** (M1 task 15). A function with no C is expanded into its callers when
there's evidence of register passing:
- a caller writes, before the call, a register the callee reads on entry;
- or a caller reads, after the call, a register the callee writes.

Registers are shared and `jr ra` dispatches over the return sites, which is
the hardware's behaviour. C-defined functions never inline. *Why:* btn needs
it only for libgte `msc02` (helper in/out via `t0`–`t2`), the BIOS GTE patch
stub, and 7 undecompiled game functions. A memory register file would cost
every translated function. (design §4)

**D34. The differential harness compares function by function.** Every
call is a logged call-out, answered identically to both sides:
- **The C side** is compiled as if alone: non-static definitions are made
  `weak`, with `-fno-inline-functions -fno-optimize-sibling-calls`.
- **Inputs follow the o32 contract:** narrow arguments are extended to
  their declared type; pointer parameters and pointer-returning callees get
  NULL or real pointers.
- **The comparison:** return values masked to the declared kind, call-out
  sequences (stack addresses equivalent, narrow arguments compared at their
  declared width, `break` events compared as a set of codes), and every
  page either side touched, apart from the stack.

*Why:* this is what isolates a single function's semantics. Each relaxation
has a recorded reason in `crates/psxw-difftest`.

**D35. Normalizer lift rules from M1:**
- functions using a GNU global register variable (`global-register`);
- functions reading a local register variable they never assign
  (`register-capture`);
- non-void functions that can fall off the end (`return-type`);
- C float arithmetic when the reference flags say `-msoft-float`
  (`soft-float`).

*Why:* in each case the C has no defined equivalent of the original's
register or soft-float behaviour. These are GNU C and GCC idioms, so they're
rules, not pins. (design §4)

**D36. GCC conventions are reproduced at link time** for C inputs (the
`Gcc` struct in `psxw-link`):
- **Divisions** give the R3000's results with the original assembler's
  `break` checks. The convention is measured from the translations: ASPSX
  checks zero with code 7; GNU as also checks INT_MIN / -1, with 6 << 10.
- **Constant-size `memcpy`** is expanded inline, as GCC's builtin did,
  unless the reference flags say `-fno-builtin`.
- **`main` calls `__main` first.**

**D37. wasmtime 47** (the newest supporting the pinned Rust 1.94.1; 48+
need newer rustc). Raising Rust would be a separate decision.

### M2 to M13

**D38. The runtime is merged, not linked** (M2). `psx-runtime` is built
by cargo as a wasm32 `cdylib` that imports the guest's memory
(`--import-memory=guest,memory`) and places its data at `RUNTIME_BASE`
(`--global-base`). `psxw-link` links only the guest (C and lifted
objects, overlay stubs, dispatch, safepoints). binaryen `wasm-merge` then
binds the guest's `psx.*` imports to the runtime's exports and the
runtime's `guest.*` imports to the guest's exports, and `wasm-opt -O2`
inlines across the seam. Each module keeps its own function table.

*Why:* rustc's own wasm-ld handles every relocation Rust emits, so
`psxw-link` doesn't have to (M2 task 2's "accept Rust objects"). The
runtime stays swappable by re-merging, with no guest relink. The cost is
binaryen in the toolchain pins, which M0 already has.

**D39. Whole-game RAM oracle conventions** (M3 tasks 8–9):
- **Alignment** is on values of the manifest's `[oracle] frame_counter`.
  psx-wasm runs once to trace it and pick values (by default the counter
  at every 600th frame), then both sides dump RAM at the first frame the
  counter holds each value.
- **Always excluded:** kernel RAM (`0x0`–`0xFFFF`: the HLE BIOS and
  OpenBIOS keep different state there by construction) and the stack (64
  KiB below the initial `sp` unless `[oracle] stack_size` says otherwise).
  Manifest exclusions name a symbol and a reason.
- **Attribution:** an unsized symbol extends to the next symbol (at most 64
  KiB); overlay addresses use the symbols of whichever overlay's code is
  resident in that dump; anything else is reported by 4 KiB page.
- **Replays can be written as presses** (`BUTTON@FRAME[:HOLD]`), so a
  game's default replay lives in its manifest as text.

**D40. Pad input runs through the original libpad over a register-level
SIO0** (M4). `sdk_psyq::DEVICE_LIBRARIES` names libraries that get no
adapters. Their functions' I/O-window accesses call the runtime
(`psx.io_access`), whose bus (`psx_bios::io`) has the interrupt controller,
the root counters and SIO0. SIO0 is a bit-level port of the oracle's
FrontIO and digital pad. Interrupts walk the `SysEnqIntRP` chains as
OpenBIOS does.

*Why:* conformance against the translated library showed that what games
observe is libpad's own state machine: PadGetState goes 0 → 1 → 2 over
the first frames, the buffers start `FF 00`, PadStopCom returns 0, and
port 2 is first polled a vblank after port 1. An adapter would have to
reimplement that per libpad version. The original over the device is
exact by construction, and costs a few SIO0 bytes per frame. Other
functions keep adapters. A debug build still traps on their I/O
(`psx.io_trap`). (The general rule stays §5: adapters for what touches
hardware. This is the exception where the hardware is small and the
library's logic is the behaviour.)

**D41. Q2 decided: a register-level SPU is the fallback for unknown libspu
versions** (M8). M6's `psx-devices::spu` models the SPU at register level
anyway: its oracle vectors (`SPU_RUN`) are register writes. A libspu object
with no validated adapter therefore gets `policy = "device"` in the
boundary: the original library runs over the SPU registers, as libpad does
over SIO0 (D40). Known objects keep their adapters, which are cheaper per
call. If conformance shows the adapters can't match libspu's observable
state, the device route becomes the default for libspu too.

*Why:* the register model has to exist for the oracle in any case, and
D40 showed the device route is exact by construction.

**D42. libgpu runs as original code over the GPU front end and a DMA
controller** (M4). The M4 GPU front end (`psx_devices::gpu`) is
register-level: it takes GP0/GP1 words and answers GPUSTAT/GPUREAD. So
libgpu became a device library like libpad (D40), and its 23 adapters were
removed.
- **The bus** (`psx-bios::io`) gained the DMA controller
  (`psx_devices::dma`). It handles channel 2 in block and linked-list
  modes and channel 6 (ordering-table clear), completing instantly in
  virtual time, setting DICR and raising IRQ3. libetc's adapter gets
  completions through `Sdk::on_dma`.
- **The host renderer** gets the front end's output at each vblank and at
  every VRAM→CPU transfer (`Host::render_submit`/`render_readback`/
  `present`, the M4 render imports).
- **Cost.** `DrawOTag` is one register write: the linked list is walked
  in Rust, so it costs what an adapter would.

*Why:* exact by construction, and version-independent: libgpu's own code
decides every word. `psxw conform btn --lib libgpu --render server`
draws a background fill and a tile through the original library, and
reads both back exactly.

**D43. Memory cards: libmcrd runs as guest code over an HLE BIOS card
subsystem** (M7). The BIOS's card functions are documented in full
(psx-spx: `bu00:` file functions, raw sector I/O, events and their pacing).
libmcrd's own state machine (asynchronous commands, `MemCardSync`, the
new-card handshake) is what games observe. As with D40/D42, an adapter
would reimplement it; the original runs unchanged over `psx-bios::card`.
- **The card image is raw 128 KiB** (`.mcr`) in host storage.
- **Slot 1 always has a card** (formatted if no image exists). Slot 2 has
  one only if its image exists, matching the oracle's default.
- **libcard's thin BIOS wrappers keep adapters**: their `_patch_card`
  patches Sony BIOS code in kernel RAM.

**D44. Browser tests: Debian-snapshot browsers, results POSTed by the
page** (M5). Two deviations from the plan's Playwright image and
TypeScript shell:
- **The test image is `psxw-browsers`:** Chromium and Firefox ESR from the
  pinned Debian snapshot, on the oracle images' base, with no network.
  Chromium runs headless on SwiftShader. Firefox runs on Xvfb with GLX
  over llvmpipe, because headless Firefox finds no GL driver. A
  test page runs on its own and POSTs its result to a small in-container
  server; a script opens each browser headless and waits for that file.
- **The web shell is plain ES modules,** with no build step.

*Why:* it reuses the pins the oracle images already have and needs no
Node toolchain. The image is much smaller than Playwright's, which
matters on this machine. The tests only need a page to load and report,
not to be scripted.

**D45. libetc runs as original code; the BIOS's custom exception exit and
ReturnFromException are wasm exceptions** (M4). libetc's interrupt layer
isn't a set of register writes. `startIntr` calls `setjmp`, points the
saved sp at a private stack, and installs the buffer with `HookEntryInt`
(B0h:19h). After the `SysEnqIntRP` chains, the BIOS longjmps there with
r2 = 1. libetc's dispatcher runs the callbacks and ends in
`ReturnFromException` (B0h:17h), which never returns (psx-spx "BIOS
Interrupt/Exception Handling"). Translated code can follow this with
three lowerings, keyed on BIOS function numbers, not names:
- **`setjmp`** (a call to an A0h:13h trampoline, or the vector with
  t1 = 13h): the caller stores its own ra, sp, fp, s0-s7 and gp into the
  buffer, in PsyQ's layout, and gets 0. The BIOS can't see a translated
  caller's registers.
- **The return address becomes a continuation entry**, a lift-tier
  function (`Source::Continuation`) entered with the buffer and the
  longjmp value. It reloads those registers.
- **`ReturnFromException`** calls `psx.return_from_exception` and then
  throws the guest's one tag. `guest_invoke_irq` (psxw-link) catches it,
  and restores `$sp`/`$gp` as the BIOS restores the interrupted context.

The runtime runs each exception as `IrqStep`s: every chain element, then
the exit. An RFE skips the rest of that exception; an exit that returns
normally is a fault. The default exit (no hook) is to return.

Like D40/D42, libetc became a device library and its 15 adapters were
removed. Measured against them first, the original matched on every
vblank count, wait, video-mode and callback observation in `psxw conform
btn --lib libetc` (59 steps). The adapters differed only where they had
cut corners: `ResetCallback`'s first return (libetc's environment
pointer, not 0) and `GetIntrMask` (9, not 0).
- **Cost:** ABI 4. Every host needs wasm exception handling (exnref):
  wasmtime's `gc-drc` feature, and Chromium and Firefox ESR in the pinned
  images both compile the module. One throw per delivered interrupt.

*Why:* the same reason as D40/D42. The original decides every observable
value, version-independently. The lowerings are generic BIOS semantics
that any SDK using `HookEntryInt` gets for free.

**D46. Q1: browser tab only, for now** (M5). The default when nobody
asks for a desktop shell, taken without the human's input as instructed
(the human can reopen it). The bundle already runs in any
cross-origin-isolated tab served with COOP/COEP (`psxw serve`). A Tauri
shell would only add a window around the same bundle and a packaging
toolchain to pin. M5 task 7 is not done.

*Why:* nothing in M5-M8 needs a native window, and the native host
(`psxw run`) covers headless use.

**D47. libspu and libsnd run as original code over the register-level
SPU** (M6). `psx-spu` matches the oracle's SPU vectors. It sits on the bus
at 1F801C00h, with DMA4 and IRQ9, and runs on its own 44.1 kHz clock. It
is brought up to the current cycle before every access, every DMA4
transfer and every vblank, and each tick takes the CD's output frame as
its CD input. Its mix is what `host.audio` carries.
- **libspu:** its 26 stub adapters (transfers "complete", keys report
  off) are removed, and its own code programs the SPU. D41 made that
  the fallback for unknown libspu versions; with an exact device it is
  simply the way.
- **libsnd** stays original code, as before, but becomes a device
  library: `_SsInit` and others write SPU registers directly, and those
  writes must reach the SPU.
- **Checks:**
  - `psxw conform btn --lib libspu` uploads a looping sample with
    `SpuWritePartly`, sets master and voice attributes, and keys the
    voice on. The run must be audible (22,085 of 22,117 frames are).
  - The libsnd tick check still finds 60 and 240 ticks.
  - `web-test conform` replays libspu in both browsers with the same
    audio hash.

*Why:* as D40/D42/D45. With the device exact, the original library
decides everything a game can observe, and nothing is reimplemented.

**D48. The whole-game RAM oracle is a diagnostic while CD timing is
unresolved** (M4 task 11, M6 task 10, M7 task 6). Aligning on the game's
frame counter was meant to absorb CD load time (M3). For btn it can't.
- **What the disc runs showed:**
  - `RunSceneUpdateLoop` calls `Rand(0xFFFF)` on every pass. While the
    loading display is up, passes don't advance the frame counter, so
    the RNG steps once per two vblanks of every load.
  - Everything drawn from the RNG afterwards differs. For example, ovl_27
    copies one of three sprite sets into `gAuxSpriteHeader` by `Rand(3)`:
    6 KB differ from counter 509 on.
  - Loads take longer under Beetle than under our realistic model. The
    counter stalls for 33 vs 95 frames at 728, 110 vs 182 at 791, and
    10 vs 256 at 1003.
  - Beetle also adds random jitter to every command's latency, from its
    own seeded generator.
- **Consequence:**
  - No drive model short of Beetle's own makes the RAM equal.
  - On hardware, load times vary from run to run, so the same holds
    against M10.
- **Decision:**
  - `psxw oracle ramdiff` stays as built (align, compare by symbol,
    report) and is used as a diagnostic.
  - The "zero differences" tasks are blocked on CD timing, which D23
    leaves unresolved.
  - The other difference classes it shows are recorded in M4's Findings
    for later: libcard's BIOS-dependent patching, libetc's interrupt
    stack, libcd's internal state under adapters, and `Vcount`.
- **Not taken:**
  - Patching Beetle to record its CD controller events, so psx-wasm
    could replay them. The emulator stays an unmodified pinned build;
    the human ruled out patching it for instrumentation.
  - Moving libcd to original code over a register-level CD controller
    only to remove its internal-state differences.
- **What still gates:** determinism (two runs give identical RAM, frames
  and audio), the boot checks, device vectors and conformance.

*Why:* exact whole-game equality would compare two drive models, not the
port. The machinery to force it would cost more than it tells us.

**D49. Mods hook function boundaries, declared outside the code** (M11).
- **Declarations:** a mod's `mod.json` states every hook: target, kind
  and implementing function, each distinct aspect its own field (no value
  packs several facts into one string). Mod code is ordinary functions;
  nothing in it marks a hook.
- **Conflicts warn, and the last applied mod wins:** each applied mod
  wraps what is already there, so the last applied is outermost. Errors
  are reserved for a mod's own defects.
- **Hooks:**
  - kinds are `before`, `after` and `patch` (optionally with
    `original`);
  - `before`/`after` are a `patch` with `original` that the build writes;
  - `from` limits a hook to one caller;
  - data patches can store a mod function's address.
- **No runtime events:** names such as `frame` are game-manifest aliases
  for real function boundaries.
- **No memory or value watches:** a mod reacts to a value with ordinary
  hooks.
- **Any language:** mods in any language that compiles to wasm get the
  same guest pass as game code (address masking, their own area of the
  window).

*Why:* every hook and every conflict between mods can be found without
running anything. Every hook has the semantics the game itself defines,
and the API is the same in every language.

**D50. Game C keeps GCC 2.95's call structure** (M11). Every non-inline
function of the game's C compiles `weak` and `noinline`, in every build:
- clang neither inlines its calls nor uses its body to fold, specialize
  or delete them, which is what GCC 2.95 at `-O2` did with a function not
  declared `inline`;
- `static` functions lose `static` for it (a weak definition can't be
  internal); the linker resolves by address, not name, so nothing
  collides;
- `inline` functions are left alone: GCC inlined them too.

*Why:* a hook must fire on every call the original program makes, and
hooking must not change the game's timing. Safepoints are spent per wasm
loop header, so with clang's inlining, recompiling a hooked function's
translation unit changed where loops spent the budget, hence when
interrupts arrived: a pass-through hook on every C function diverged from
the plain build by frame 210 of `title-to-gameplay`. With the call
structure fixed in every build, the plain build and the pass-through are
bit-identical. Its cost is within run-to-run noise on that replay: 6000
headless frames took 166 and 169 s with inlining, 174 and 168 s
without.

**D51. Mods loaded at run time go through hook slots in a moddable base**
(M11 phase 2).
- `psxw build --moddable` gives every hookable function an entry that
  checks its slot in an exported table; without mods it runs as the plain
  build.
- A mod module is linked by psxw from the same objects (or from any
  toolchain's `wasm-ld --emit-relocs` module), with the same hook layers.
  Its data is addressed relative to the area the loader gives it, and its
  functions' addresses relative to a synthetic range.
- The host wires chains wasm-to-wasm through the base's tables. A layer's
  next link is a slot of the same table, so a swap only rewires.
- **`from` hooks are static only.** The loader refuses a module with
  them: telling callers apart at run time would need a slot per call
  site in the base, a check on every call for a rarely used modifier.
  `psxw mod build --modules` says so and names the hook.

*Why:* the static and dynamic backends must agree bit for bit, and JSPI
can't suspend through a JS frame between two wasm instances.

**D52. Start live scripting with one queued guest call.** The browser
exposes `window.wasmPsxScripting.execute(address, ...args)` to call existing
compiled code, with up to four o32 argument words and the raw u64 result.
It has no persistent hooks, typed state, code loader or symbol service.
The runtime owns a single request slot outside its borrowed machine; the
driver executes it after draining callbacks, outside borrows, IRQs and
critical sections. Results use `host.report`. `$sp`/`$gp` are preserved.
The browser requires JSPI and the merged module; invalid guest calls can
still trap or hang the game. ABI 8 adds the queueing export.

*Why:* the human requested the smallest single-poke proof. Calling
`psxw_call` directly while JSPI suspends `wait_vblank` would re-enter the
runtime while its machine is mutably borrowed. Existing symbol addresses
suffice for references; debugging source maps are a separate concern.

*Later:* M12 renamed the page's API `psx` (`psx.execute`), and script
mods' commands and `psx.run` use the same slot (D54).

**D53. A debug build carries the C's DWARF to the browser, at addresses a
debugger can read.** `psxw build <game> --debug` gives `game-debug.wasm`,
and `psxw build-web <game> --debug` gives `out/web/<game>-debug`.
- **The C is compiled with `-g`** into its own directories
  (`out/<game>/debug/`), lifted C included. A translated function then
  shows its generated C, one MIPS instruction a line. `-g` doesn't change
  clang's code (sampled: 95 of 95 objects identical).
- **psxw-link carries the DWARF through the guest passes**
  (`psxw_link::dwarf`). The rewrite records where each input instruction
  lands. Every code address, length, range, location list and line row
  goes through that map, data addresses are written as the link placed
  them, and `__stack_pointer` becomes `$sp`.
- **The window moves to `DEBUG_WINDOW_BASE` = `0x8000_0000`.** Every KSEG0
  address is then its own wasm address, so a DWARF debugger reads globals,
  stack slots and what pointers point at where the game put them. The
  memory is declared from 0 to the window's end (33,024 pages, about
  2 GiB). Nothing touches the gap, and hosts don't commit it: a native
  boot peaked at 163 MB, against 184 MB for the plain build. The guest
  exports `guest_window` (ABI 9); the runtime and both hosts read it, and
  the JS host treats pointers as unsigned, since the runtime's heap now
  starts above 2 GiB.
- **psxw-link merges the modules, not binaryen** (`psxw_link::merge`).
  Binaryen 133 rejects DWARF 5. With DWARF 4, `wasm-merge` zeroes every
  address, and `wasm-opt -O2` invalidates DWARF anyway. Our merge does
  what `wasm-merge` does with these two modules. It re-encodes the guest's
  code changing only call targets, maps where it moved, and applies that
  map to the DWARF. No `wasm-opt` runs: clang already optimized the C.
- **The source paths are relative, and the bundle serves the files.**
  The decomp's C compiles with `-fdebug-compilation-dir=source/game`, and
  the generated C with `source/lift` and paths relative to the lift
  directory. The extension resolves relative paths against `game.wasm`'s
  URL. `build-web --debug` copies every file the line tables name into
  `source/`, so the C loads from the server on any machine, with no file
  access. Absolute paths became `file://` URLs, which a browser on
  another machine couldn't open. The extension also fetches `game.wasm`
  itself, from its own origin, so `psxw serve` sends CORP `cross-origin`:
  `same-origin` blocked that fetch, and DevTools listed no C at all.
- The debug build's guest RAM and audio hashes equal the plain build's
  at frames 60, 300 and 600.

Limits:
- **Some pointers still read wrong in the debugger.** Scratchpad
  (`0x1F80_xxxx`) and KUSEG/KSEG1 pointers point into the unused gap, so
  the debugger shows zeros. The game itself runs correctly.
- **`-O2` C steps unevenly**, and some variables are optimized out.
- **It doesn't combine with `--moddable`, `--ubsan` or mods yet.**

*Why:* the human wants C-level debugging in DevTools, variables included.
Source maps carry only lines, so they were rejected. Chrome's C/C++
DevTools Support extension (DWARF) reads pointers as wasm addresses,
which only this layout makes true.

**D54. Script mods are JavaScript layers on the moddable base** (M12).
- **A script mod is one ES module.** Its default export, given the mod's
  `game` and the mod API, returns the mod's definition:
  - hooks: `before`, `after`, `replace`, and `around`, which calls the
    next link;
  - data patches and commands.

  The loader reads the definition before installing anything. A script
  mod has no `mod.json` and no build step.
- **Shims make it a layer like any other.** The browser loader writes a
  small wasm shim for each hooked slot. The shim becomes a layer in the
  slot's chain, beside compiled mods' layers (D51), with M11's chains,
  load order and conflict rules (D49). `from` stays static-only (D51).
- **The runtime holds the host's vblank wait while JS is on the stack**
  (ABI 10).
  - `host.vblank(n)` never suspends, and carries everything the guest can
    see: the replay frame, swaps, queued calls.
  - `host.wait_vblank(n)` only paces and yields.
  - Shims bracket each call into JS with `psxw_js_enter`/`psxw_js_leave`.
    Held waits are made in order once the outermost JS frame returns.
  - Guest state is unchanged; only the page's pacing slips.
- **Commands and `psx.run` run inside the game** through D52's queued
  call, at synthetic addresses.
- **Browser only.** The native host runs no JS, so script mods' tests run
  in headless Chromium and Firefox.
- **Compiled mods stay,** and both kinds load together (deprecated since,
  D56).

*Why:* the human wants mods without a compile step, editable live from
the console. A JS hook costs about 16 ns a call on a JSPI stack (measured
in V8 13.6), so its cost depends only on how often the hooked function is
called. But JSPI can't suspend through a JS frame (`SuspendError`,
measured), and a vblank, and with it the host's wait, can be delivered at
any safepoint in any game code a hook calls. Both hosts keyed replay
input on that wait, so delaying it whole would have changed the game's
input.

**D55. Mod types come from snapshots keyed by the game binary** (M12).
- **A snapshot is neutral JSON.** It records one source's view of one game
  binary:
  - the identity hashes;
  - functions and globals by container and address, with names and
    types;
  - record layouts;
  - type expressions as data.
- **Frontends produce snapshots.** There are frontends for symbol lists,
  C headers (the header index with the layout probe) and DWARF, and the
  mods repo can add its own fragments. Only a frontend knows its source's
  conventions.
- **`game` and `game.d.ts` are generated from snapshots,** never from a
  source directly.
- **Each mod resolves names through the snapshot its workspace pinned,**
  down to addresses and offsets. A bundle needs only the same binary, so
  mods pinned to different snapshots load together.
- **`--bump` compares snapshots by address.** Renames become deprecated
  aliases, and layout changes are reported where the mod reaches them.
- **Several sources merge by address,** in an order the workspace states.
  Every source's names become aliases, and type disagreements warn.

*Why:* the human wants many iterations of a decomp, and different
decomps, supported together. Decomps rename symbols, retype data and move
code from asm to C, but the binary doesn't change, so it is the one
fixed point to key on.

**D56. Compiled mods are deprecated; script mods are the modkit** (M12).
- **New mods are script mods.** `psxw mod new` makes one (`--compiled`
  for M11's kind); docs lead with them.
- **Compiled mods keep working** where they work now: linked in, loaded
  into the moddable base, their tests natively, the `mods` gate's M11
  checks, and as the oracle script twins are held to (`same_as`).
- **New capabilities are script-only.** A debug build's moddable base
  (D53) loads script mods; the loader refuses compiled modules there,
  since their code has the default window's address built in.

*Why:* the human: once script mods worked, the compile-and-reload flow is
the old one. Script mods cover what compiled ones do with no build step,
bit for bit (M12's twins), and a script hook's cost (about 100 ns a call)
matters only for the hottest functions.

**D57. Guest threads switch stacks with Asyncify, scoped by the call
graph** (M13). ChangeThread (B0h:10h) must suspend one guest call stack
and resume another, and a translated function's MIPS registers are wasm
locals, so the wasm stack itself switches.
- **What a game does with threads** (bof4, measured): a round-robin
  scheduler on the main thread opens up to four worker threads
  (OpenThread with the entry, a 200h-byte stack and gp from a table) and
  ChangeThreads to each in turn; a worker yields with
  `ChangeThread(0xFF000000)` from any depth, or closes itself. The game
  also edits the TCBs directly (each thread's saved SR, enabling COP2), so
  the kernel's tables live in guest RAM (psxw-abi `kernel`: the table of
  tables' PCB and TCB entries, the PCB and the TCBs at E000h, sized by
  SYSTEM.CNF `TCB=`). Events stay in the runtime.
- **How:** binaryen Asyncify (pinned, version 133) instruments the guest
  after the link, only the functions that can be on the stack at a
  switch: the reverse closure of the functions making B0h:10h calls,
  through direct calls, plus every function making an indirect call (an
  indirect call can reach any function in the dispatch table). For bof4
  that's about 13% of functions. `psx.bios` is the one import that
  unwinds. Functions with exception handlers (`guest_invoke_irq`) can't be
  instrumented and are left out; a switch from a callback or interrupt
  handler traps.
- **The runtime's side:** ChangeThread saves the running thread's sp and
  gp in its TCB and records the target; `bios` then unwinds the thread
  into its buffer, back to the thread's root call in `run_threads`, which
  starts the target afresh at its TCB's start address (and sp, gp, which
  the game may have edited) or rewinds the target's own stack to the
  ChangeThread it called, which returns 1 there.
- **A game that never switches** gets stubs for Asyncify's exports, so
  its module is unchanged otherwise (ABI 11).
- **Not taken:** wasm stack switching (host support is uneven, and it
  would split the browser host); one instance per thread over shared
  memory (a shared-memory build, and per-instance globals synced on every
  switch).

*Why:* the plan's recommendation, taken without the human's input as
instructed (the human may reopen it). It works the same under wasmtime and
in browsers, is deterministic, and costs only in the functions that can
be suspended.

**D58. A register-level CD controller for libds and libcd's internals;
`adapter-or-device` decides per library** (M13). bof4 links libds (no
adapters exist) and a libcd whose drive layer (`CD_init`, `CD_cw`, the
`St*` streaming ring) its game code and libds call directly.
- **The device:** `psx_devices::cdc`, at 1F801800h-1F801803h with IRQ2
  and DMA3. Commands get INT3 after the drive model's latency and INT2
  when the drive has done the work; one interrupt is presented at a time;
  reads deliver a sector per period on the virtual timeline (realistic or
  instant timing, as `cd_timing` says), into the buffer the data FIFO or
  DMA3 reads; XA-ADPCM sectors go to the existing decoder and into the
  SPU's CD input. Sectors are checked by EDC. No CD-DA (bof4's second
  track is data).
- **The policy** `adapter-or-device` (libcd): decided per game for the
  whole library, never per object, because the adapters keep the drive's
  state in the runtime, where the library's own code can't see it (a
  `CdDataCallback` adapter, say, doesn't register libetc's DMA3 callback
  that libcd's original code relies on). Adapters bind when every placed
  object defining an adapter's function is validated and the code outside
  the library (the game and other libraries, through direct calls,
  stopping at those adapters) reaches its other objects only at helpers
  that call nothing (`CdIntToPos`). Otherwise the whole library runs as
  original code over the controller. btn keeps its adapters; bof4's libcd
  is reached past them at `CD_cw`, `CD_init`, `StSetRing` and others.
- **libds** is `device`; **libpress** is `device` over the MDEC
  (`psx_devices::mdec`, DMA0 in, DMA1 out, decoding instantly in virtual
  time).

*Why:* the plan's recommendation (b), taken without the human's input as
instructed. It follows D40, D42, D45 and D47: the original library decides
everything observable. The per-library rule is this milestone's
refinement of the recommended per-object one, which would have mixed
adapter state with original code in bof4.

*Later:* D59 decides per chip, and D62 moved libcd's adapters to its drive
layer: both games run it native, the controller is the fallback.

**D59. Adapters first, the device as the fallback, decided per chip**
(unified adapter layer, phase 1). Each chip runs native through SDK
adapters wherever a validated adapter set covers everything a game uses
on it. Otherwise the whole chip falls back to its register-level device
model.
- **Chip groups:** the boundary file names each chip's libraries (CD:
  libcd, libds; SPU: libspu, libsnd; GPU: libgpu; SIO0: libpad; IRQ and
  timers: libetc, libapi's counters; MDEC: libpress). The build decides
  each chip, not each library: a chip can't have two drivers.
- **Native** when every placed object in the group that defines an
  adapter's function is validated, and nothing the game runs reaches the
  chip outside the adapter set: no function reached from outside the
  group (through direct calls, stopping at adapters) touches the chip's
  registers, or drives an adapted library's internals.
- **One adapter per SDK function,** the same code in every game: no
  per-game variants, tables or modes. If an adapter works in one game
  and not another, its seam or its shared state is widened.
- **Two kinds of adapter.** Hardware adapters fall back to the device.
  BIOS-internals adapters (libcard's `_patch_card`, libapi's pad flags)
  are mandatory: there is no Sony BIOS to emulate.
- **Reporting:** `build.json` says, per chip, native or emulated and
  why; `game.toml` lists the chips a game expects native, and a build
  that falls back on one fails. `--device <chip,…|all>` forces chips onto
  their devices, for conformance (level 4) and as the escape hatch.
- **Conformance:** an adapter is done when it matches the original
  library over the device models on every observation code staying
  original can make: per call, per chip sequence, in every game that
  links it, and whole-game replays in both modes.

*Why:* the human's decision ("graceful degradation to emulated
hardware"). It reverses the device default of D40, D42, D45 and D47,
which made the layer beneath the SDK an emulator, and generalizes D41
and D58 (libspu's and libcd's adapter-or-device policies) from one
library to every chip. The device models stay, as the fallback and as
the oracle. (plans/unified-adaptor-layer)

**D60. libpad's adapters run libpad's engine, including its time**
(unified adapter layer, phase 1 task 9c). D40 kept libpad original
because games observe its state machine. Chip sio0's adapters reproduce
that machine instead of approximating it:
- **The seam** is the calls games make (`PadInitDirect`, `PadStartCom`,
  `PadGetState`, `PadInfoMode`, `PadSetAct`, `PadSetActAlign`) plus
  `PadStopCom`, which shares `PadStartCom`'s element. Their state (the
  port records, receive buffers, pending replies) is in the runtime; the
  games' buffers are guest RAM, written as the original writes them.
- **The engine** is libpad's direct-connection code, from the 4.7
  objects: per vblank, each port's pending reply processed, then an
  exchange whose bytes a digital-pad model answers (SIO0's device model
  is a digital pad too). Replies land a port late, detection asks for
  config mode twelve times, failures count, as on the original.
- **The interrupt is a kernel chain element the adapters own**
  (`Sdk::chain_element`): `PadStartCom` queues an address no game code
  has at priority 2, and the kernel runs it in its place in the chain.
  The pads latch their buttons at the vblank, as SIO0's do.
- **Its busy-waits take their time.** Each of the original's waits on
  SIO0 or root counter 2 ends at the next safepoint, a quantum later,
  inside the interrupt: half a frame with two pads, where hardware takes
  about 20,000 cycles. The adapters spend the same quanta, delivering
  nothing, and then start the guest's safepoint budget over as the
  original's last safepoint does (psxw-abi `guest::BUDGET_RESET`).

*Why:* conformance against the device is exact on every observation,
per vblank (buffers, states, types, failure counts, where the game
resumes in the frame). bof4's `title-to-field` showed the time is
observable: with no time spent its counter fell 322 frames behind and
two expects failed; with the time but the budget left as it was, the
game took another path from frame 110 (its main loop does up to a
quantum's iterations less a frame); with both, btn's and bof4's replays
match the device frame for frame. The same rule as D59's for `VSync`:
an adapter that replaces a busy-wait waits the way it does. The device
models only a digital pad, so the extended commands (analog modes,
actuators) stay unexercised on both sides.

**D61. An adapter finds its object's data through the object's own code,
and calls the guest the way that code does** (unified adapter layer, task
9b). libgpu keeps its state in its own data: `GEnv` (graph type, queue
switch, VRAM size, the DrawSync callback, the last DRAWENV and DISPENV),
the command queue, and packets (ClearImage's fill, MoveImage's copy, the
ordering table's terminator). Code that stays original reads it: the
draw-area helpers `SetDrawArea` and `SetDrawEnv` call (`sys:0x1650`,
`sys:0x16e8`, static functions each game names differently), `DrawSyncCallback`, `ClearOTag`, `GetDrawEnv`, the
font functions. So libgpu's adapters keep that state where libgpu keeps it
(D59's second option), and need its addresses, which differ per game.
- **The replaced function's address.** The adapter thunk passes it
  (`psx.adapter(id, addr, a0..a3)`, ABI 12), and the runtime gives it to
  the adapter (`Psyq::at`). The object is a validated one, so the
  function's place in its text is known, and the relocated operands of
  its code (a `lui`/`lo16` pair, a `jal`) give the object's `.data`,
  `.bss` and `_que`, and the functions it calls in other libraries
  (`ResetCallback`, `GetVideoMode`, `DMACallback`). The identity key fixes
  every other bit of those instructions; an operand that isn't where the
  object has it is an error. No per-game table.
- **Nested guest calls.** An adapter can call a guest function and go on
  with its result (`Flow::Call`), as the original's `jal` does:
  `ResetGraph` calls `ResetCallback`, `PutDispEnv` `GetVideoMode`, the
  queue `DMACallback`, and the queue's pump the DrawSync callback, inside
  the call that drained the queue, with the callback's own address in
  `a0` as libgpu leaves it. Callbacks and
  libetc's state land where the original puts them, whichever way chip
  irq runs. libgpu's pump, which it makes DMA2's callback, is a stand-in
  function (`libgpu::PUMP`), like libetc's own handlers.
- **Shared devices stay shared.** The GPU's transfers are DMA2 and DMA6 on
  the DMA controller, programmed as libgpu programs them and with the
  controller's semantics (as chip irq uses I_MASK and DICR): DPCR gates
  them, DICR and IRQ3 reach whatever DMA callbacks are set, a transfer
  DPCR doesn't enable waits for it, and the GPU recording hook still sees
  linked lists. GP0/GP1 words go to the front end directly.
- **Waits spin like libgpu's loops,** a safepoint quantum per thousand
  iterations, counted toward libgpu's timeout (0xF0000 iterations or 240
  vblanks), which resets the queue and the GPU as libgpu does: btn's boot
  draws before `ResetGraph`, with DMA2 off, and times out at the same
  frame natively as on the device.

*Why:* keeping libgpu's state in the runtime would need an adapter for
every accessor, and two are static functions the build can't bind by
name. The identity key makes an object's code operands as fixed as a
struct layout. Calling the guest where the original does keeps callback
order, arguments and timing without the gpu adapters knowing how libetc
runs.

**D62. The CD's seam is libcd's drive layer, its state kept where libcd
keeps it** (unified adapter layer, phase 1 task 9d). Chip cd's adapters
are the functions that reach the CD controller or DMA3 themselves:
`bios_1`'s `CD_init`, `CD_initintr`, `CD_cw`, `CD_sync`, `CD_ready`,
`CD_flush`, `CD_vol`, `CD_initvol` and `CD_datasync`, with the interrupt
handler they install on line 2; `bios_2`'s `CD_getsector` and `bios_3`'s
`CD_getsector2`; and the streaming ring's `StCdInterrupt` and
`StUnSetRing`. libcd's public layer (`CdInit`, `CdControl*`, `CdRead*`,
`CdSearchFile`, the callbacks and accessors), libds and the rest of the
ring are original code over them, in every game.
- **One adapter set.** btn calls the public layer, bof4 libds and the
  ring, and both link the same drive-layer objects (one identity key
  each), so the same adapters serve both. The 22 public-layer adapters
  btn had are gone.
- **State in the library's own globals** (`bios_1`'s `.data` and `.bss`,
  the ring's variables): libds writes the drive layer's callback slots
  itself (`DS_init`, `DS_close`), the public layer reads its status, mode,
  last command and position, and callbacks get pointers to its result
  buffers. So every accessor stays original.
- **Found through the object's own code,** as libgpu's adapters find
  theirs (D61): from the guest address it replaces (`Psyq::at`), and the
  object's layout the identity key fixes, an adapter reads its globals'
  addresses, and its callees', from its own relocated `lui`/low-16 pairs
  and `jal`s at known offsets. It makes the original's guest calls
  (`ResetCallback`, `InterruptCallback`, `CdReady`, `init_ring_status`,
  `StFunc2`) as nested calls (`Flow::Call`).
- **One controller.** The adapters drive the CD controller's core
  (`psx_devices::cdc`, D58) without its register protocol, so native and
  emulated libcd see one controller with one timing. Its interrupt is
  IRQ2; line 2's handler is a stand-in function installed through the
  guest's own `InterruptCallback`, so it runs whichever way libetc does.
  Sectors move by DMA3 through the DMA controller's registers, which the
  chips share (as chip irq's adapters use DPCR and DICR), so DMA3's
  callbacks arrive as on the device. `CD_initvol`'s SPU writes go to the
  SPU device's registers until chip spu has adapters.
- **Waits spin** a safepoint quantum at a time (D59) and, inside a
  callback, poll the controller as the original does.

*Why:* the low layer is where the two games meet, so principle 1 holds by
construction and nothing above it needs an adapter. The other choice,
public adapters and low-layer adapters over one drive state in the
runtime, would have needed adapters for every accessor and for libds's
own writes of the callback slots, and a reimplementation of `CdRead`'s and
`CdSearchFile`'s retries and callbacks. Sharing the controller's core
makes commands, interrupts and sectors arrive at the same cycle in both
modes, so conformance compares only the library logic. Decided without
the human's input, as the task said (plans/unified-adaptor-layer, phase 1
findings).

**D63. libspu's seam is where it touches the SPU; its adapters drive a
mixer API and spend the original's time** (unified adapter layer, phase 1
task 9f). Both games call libspu's public functions directly and from
libsnd's sequencer, which stays original (D14), and libsnd's `SsInit`
reaches the SPU through `SpuInit` and `_SsInit`.
- **The seam** is every libspu function whose own code reaches the SPU
  or DMA4 that code outside it calls: `spu.o`'s layer (`_spu_init`,
  `_spu_FsetRXX`, `_spu_FsetRXXa`, `_spu_Fw`, and `_spu_FiDMA`, DMA4's
  callback, reached through the pointer `SpuStart` gives `DMACallback`:
  an adapter bound by name, so no stand-in), the functions that program
  the SPU themselves through `_spu_RXX` (`SpuSetKey`, `SpuSetVoiceAttr`,
  `SpuSetCommonAttr`, `_SpuSetAnyVoice`/`_SpuGetAnyVoice`, the reverb
  functions, `SpuSetNoiseClock`, the key, envelope and volume readers,
  `SpuClearReverbWorkArea`), and libsnd's `_SsInit`: 21 adapters, 19
  bound in each game. libspu's and libsnd's state stays in their
  `.data`, found through the objects' code (D61), so `SpuInit`,
  `_SpuInit`, `SpuStart`, `SpuWritePartly`, `SpuSetTransferStartAddr`,
  `SpuIsTransferCompleted`, the allocator, the voice-mask wrappers,
  `SsInit` and the rest of libsnd stay original over them. `_SsInit`
  calls the original `_SsVmInit`; adapters call the guest where their
  original does (D61): `_spu_Fw1ts` (libspu's delay loop),
  `_spu_note2pitch`, `_SpuIsInAllocateArea_`, `printf`, `DeliverEvent`,
  `WaitEvent`, the transfer callback.
- **The core** is `psx-spu` through its mixer API (`psx_spu::mixer`,
  phase 1 task 7): voices, the 24-bit masks, the volumes, SPUCNT, the
  reverb registers, the transfer address and FIFO, each setter the
  register write it stands for, so a voice programmed through it is the
  register path's voice by construction; `Machine::with_spu` brings the
  SPU up to now first, as a register access does. `_spu_FsetRXX`'s
  registers by index map onto it. DMA4's words go through the core's
  transfer port, MADR4/BCR4 on the shared controller, and its completion
  is held (D64).
- **Time is the translated original's.** Its loops spend the guest's
  safepoint budget, and so do the adapters: they read and set it (new
  guest exports `guest_budget_get`/`guest_budget_set`, psxw-abi
  `guest::BUDGET_GET`/`SET`, `Regs::budget`/`set_budget`) and spend the
  passes the original's loops make, with a safepoint where it runs out,
  at the same place among their SPU writes. The passes are the
  translation's, measured against the original over the device with the
  budget observed after every conformance call: clang unrolls short
  constant loops (the register queue's 10 halfwords, `_SsInit`'s 16
  registers and its mark table's inner loop, `_spu_init`'s voices two at
  a time, the reverb parameters a word at a time) and makes
  `SpuGetKeyStatus`'s search a count of trailing zeros. The delay loop is
  the original's own code. And an adapter's return is no safepoint
  (`Machine::quiet`, which the driver takes at the adapter's return):
  the original makes no runtime call there.
- **Observed** in conformance: the SPU's whole state, hashed at the core
  (`Spu::state_hash`, the runtime export `psxw_spu_hash`, in
  `Harness::device` with the audio), the budget left after every call,
  and RAM below the cases' scratch in 64 KB blocks.

*Why:* with the state in guest RAM nothing above the seam needs an
adapter, and the deepest seam leaves the most of libspu original. Over
the shared core the native SPU is the device's to the tick: btn's 1,200
frames and bof4's `title-to-field` (25,300 frames, 18.7 million audio
frames) give the same audio hash both ways, the frame counters are equal
every frame, and conformance compares the SPU's state after every call.
Time decided it: a first `SpuInit` that spent 22 budget passes too many
moved every later safepoint, and without quiet returns btn made one
`VSync` poll more than the device from frame 395, where a transfer's
completion ran at the return of `_spu_FsetRXXa`, the adapter
`SpuWritePartly` calls after `_spu_Fw`, instead of at the guest's next
safepoint. Decided without the human's input, as the task said
(plans/unified-adaptor-layer, phase 1 findings).

**D64. libpress's adapters drive the MDEC's decode function, and hold
the interrupts its register writes raised** (unified adapter layer, phase
1 task 9e). bof4 links libpress, in an overlay, and its movie player
calls `DecDCTReset`, `DecDCTin` and `DecDCTout` (with `DecDCToutCallback`
loading each slice).
- **The seam** is every function of `libpress/libpress` that reaches the
  MDEC or DMA0/DMA1: those three, `DecDCTPutEnv`, `DecDCTinSync`,
  `DecDCToutSync`, so no function of the object reads the I/O shadow when
  the chip is native. libpress's tables stay in its `.data`, found as
  D61 finds libgpu's, so `DecDCTGetEnv` stays original; the callback
  setters call libetc's `DMACallback`, and the run-length coders
  (`DecDCTvlc*`) touch no hardware.
- **The core** is `psx_devices::mdec`'s decode function, `Mdec::input`
  (command and parameter words, decoded at once) and `Mdec::output`
  (decoded words, zeros past them), which the device's DMA0 and DMA1 now
  call too. `DecDCTin`'s words go in as libpress's DMA0 would deliver
  them (blocks of 32, at least one); DMA1 waits, as on the device, while
  a decode is still taking input and hasn't produced the words asked
  for, and `DecDCTin`, a reset or a timeout lets it go.
- **Completions are the shared DMA controller's** (DICR's flags, IRQ3),
  so the callbacks run through libetc's DMA handler, its adapters' or its
  own. CHCR0/1 stay untouched: DMA1's busy bit is the adapters' state,
  which only libpress reads.
- **The interrupt is held** (`Machine::hold_irq`, `dma_done_held`): the
  original's IRQ3, raised by a register write inside guest code, waits for
  the guest's next safepoint, while what an adapter raises would run at its
  own return. The runtime's adapter export queues held interrupts as the
  adapter returns (`release_irq`), and a wait releases them before its
  quantum.
- **Waits spin like libpress's loops** (a quantum per thousand
  iterations), to its timeout after 0x100001, which prints through the
  game's `printf` and resets the MDEC and both channels.

*Why:* bof4's player goes on decoding the next frame's run-length codes
after `DecDCTout`, and the slice callbacks come when that work reaches a
safepoint. Taken at the adapter's return, a word the player updates every
frame (`0x80116BB8`) read 10 less than on the device from the first
decoded frame. Held, `title-to-field`'s RAM matches the device at every
checkpoint except dead stack, and the renderer's command stream is the
same, the movie's slices included. The same rule as D59's and D60's: the
device decides when a callback runs. Decided without the human's input,
as the task said (plans/unified-adaptor-layer, phase 1 findings).
