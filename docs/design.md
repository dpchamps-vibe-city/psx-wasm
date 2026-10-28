# psx-wasm: design sketch

Status: **M0 and M1 implemented (2026-10-26); the M3 device oracle is in
place apart from its whole-game tasks.** The numbers in this
document were measured against `btn-decompilation@3a15992` (working tree
dirty) and `psyq-decomp@302b6ae` on that date.

## Goal

Take a matching PlayStation decompilation of a game, plus a decompilation of
the PsyQ SDK it was built with (which may be partial), and produce a
WebAssembly build that runs the game natively in a browser or webview. The
game's code is compiled to wasm, and Rust services replace the hardware
below the SDK. There is no interpreter, no dynarec, and no cycle-level
hardware model.

The first target is Harvest Moon: Back to Nature (SLUS-01115) with PsyQ 4.7.
The test of generality is that a second decompilation runs with only a
manifest, without changes to the toolchain's code.

## What "native, not an emulator" means here

| PSX part | In psx-wasm |
|---|---|
| R3000 CPU | **Gone.** clang compiles the game and SDK C to wasm. Any residual asm is translated ahead of time (the lift tier). Nothing decodes MIPS at run time. |
| Memory map | **Kept.** RAM, the scratchpad, and every symbol sit at their original addresses inside a wasm memory window, and pointer values are the PSX's own. This is a choice of data layout, not hardware emulation. |
| GTE | A fixed-point math library (`psx-gte`) that exposes the GTE's register-file API. |
| GPU | No FIFO, DMA or timing model. `DrawOTag`, `LoadImage` and the rest are implemented by a renderer that draws PSX primitive packets into a VRAM model. |
| SPU | The `libspu` API, backed by a 24-voice ADPCM mixer. |
| CD-ROM | The `libcd` API, backed by reads from the user's disc image. |
| BIOS/kernel | A Rust implementation of the A0/B0/C0 services the SDK calls. |
| Interrupts | Callbacks run at safepoints, on a deterministic virtual clock. |

Every approach needs the renderer and the mixer, because the game's output
*is* GPU packets and SPU voices. What psx-wasm avoids is modelling the
machine that consumed them.

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
- Most of the libraries that touch hardware are replaced by Rust anyway, so
  their decompilation state doesn't matter.
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
  the way the game always loaded them, through `CdRead`. The wasm build
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
| `0x0100_0000`–`0x01FF_FFFF` | **Guest window.** Window offsets `0x000000`–`0x1FFFFF` are RAM, `0x200000`–`0x7FFFFF` hold auxiliary data (literals and constant pools that only clang emits), `0x800000` is the scratchpad, `0x801000` is I/O (debug builds trap here), and `0xF00000` is a runtime-private page (GTE registers, safepoint budget). |
| `0x0200_0000`– | The runtime's own data, stack and heap, placed there by the linker. |

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

1. **Adapter**, if the function touches hardware registers or BIOS
   internals. This is a thin Rust function in the runtime (§5).
2. Otherwise **decompiled C**, if the decomp has it and it compiles under
   the host prelude.
3. Otherwise **translated**: the original MIPS, translated ahead of time
   into C by `psxw-lift` and compiled by the same clang pipeline.

| Tier | Source | Produced by |
|---|---|---|
| `adapter` | Rust, in the runtime | Chosen per function and per object identity by the boundary manifest |
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
  repo, are needed. Relocatable `.LIB` members are used only to build the
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
  - A hardware register access is a build error: that code must have an
    adapter.

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
- **A translated SDK is reusable.** Games link SDK members byte for byte, so
  the translated C is cached per SDK release, keyed by member hash, and
  reused by every game linking that member. It is derived from Sony's
  binaries, so it is built locally from the user's inputs and never
  distributed.
- **The tier report.** `psxw report tiers` lists every function's tier and
  how it changed since the last lock. At run time, the profiler reports the
  share of executed code that comes from each tier. For game code, the lift
  tier is a migration aid that shrinks as the decompilations progress. The
  SDK's handwritten asm (BIOS trampolines, GTE wrappers, `mem*`) never
  becomes C, so translation is permanent there.

### 5. The SDK: replace only what touches hardware

The game's matching link fixes the set of SDK functions. The rule from §4
applies to each one individually, never to a whole library. An SDK
function gets an adapter only if its code touches hardware registers or
BIOS internals. Everything else runs as guest code, as decompiled C or
translated, exactly like game code.

**The runtime splits into three layers**, so that SDK versions stay cheap:

```
 guest code (game + SDK code that runs as-is: libsnd, libgs, libc …)
        │ calls a replaced SDK function         │ BIOS jump / syscall
        ▼                                       ▼
 SDK adapters (thin, versioned)          BIOS (A0/B0/C0, syscall)
        └──────────────────┬────────────────────┘
                           ▼
 devices (stable, hardware-defined): gpu, spu, cd, sio, clock, gte
```

- **Devices** speak the hardware's own vocabulary: GPU packets, `Cdl` drive
  commands, SPU voice parameters, pad report bytes. That vocabulary is the
  same for PsyQ 3.x, PsyQ 4.7, PSn00bSDK or a custom engine. Nearly all the
  hard work lives here.
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
- **Every linked member is identified exactly,** by hash and signature.
  Each adapter declares the object hashes it is validated against.
- **Struct layouts are generated** from that SDK version's headers. The M0
  layout probe showed clang's wasm32 layouts equal GCC's o32 layouts.
- **An object with no adapter** is translated and cut one level lower, down
  to the BIOS call level if necessary. Failing that, the build fails, naming
  the objects that need adapters.
- **Other SDK families**, such as PSn00bSDK or Net Yaroze, would be sibling
  adapter sets over the same devices and BIOS.

The PsyQ policy, measured on the 361 SDK members btn links:

| Library | Policy |
|---|---|
| libapi | Translated. BIOS calls go to the Rust BIOS. Adapters for `counter.o` (`SetRCnt`… program the timers directly) and for `pad.o`, `patch.o` and `chclrpad.o` (they patch Sony BIOS internals in RAM). |
| libetc | Guest code over the IRQ controller, DMA and timers (D45). The BIOS enters its dispatcher through the custom exception exit (a longjmp into `startIntr`'s `setjmp`), and it leaves with ReturnFromException; the lift and link tiers lower both to wasm exceptions. |
| libgpu | Guest code over the GPU front end and DMA (D42). The front end takes GP0/GP1 words, so libgpu's own code drives it; an ordering table is one DMA walk in Rust. |
| libcd | Adapters at the device edge: `CdInit`, `CdControl*`, `CdRead*`, `CdSync`, `CdReady`, the callbacks, `CdSearchFile`. Helpers such as `CdIntToPos` stay guest code. |
| libspu | Adapters (register programming). Open question: register-level SPU emulation as the fallback for unknown versions. |
| libpad | Guest code over the SIO0 device (D40). Its state machine is the behaviour games observe, and the device is a port of the oracle's. |
| libmcrd | Guest code over the HLE BIOS card functions (D43). |
| libmcx, libcard | Adapters. |
| libsnd | Guest code. The sequencer *is* the game's music, so the original runs and only libspu beneath it is replaced. |
| libgs | Guest code. It only writes packets into RAM; `DrawOTag` is the device step. |
| libgte | Guest code, with COP2 through `psx-gte`. Hot wrappers may get adapters for speed. |
| libmath, libc | Guest code. `rand` must be the original, or the game plays differently. `memcpy` and `memset` may get adapters for speed. |

**Device libraries (D40).** A library can run as guest code over
register-level devices instead of getting adapters. Only its functions'
I/O-window accesses reach the runtime's bus (`psx-bios::io`: interrupt
controller, root counters, SIO0), and interrupts run the kernel's
`SysEnqIntRP` chains. That pays off where the hardware traffic is tiny and
the library's own logic is what games observe.

**Conformance.** `psxw conform <game> --lib L` checks the result of the
adapter rule. One scenario of SDK calls and vblanks runs on the build with
adapters and on a build where `L` is its original code over the devices.
The two must agree on what the game can observe.

Adapters declare their C prototypes. The build checks them against the SDK
headers, so a signature drift fails at link time, not at run time.

**The boundary file is `boundary/psyq.toml`** (M8). It gives each library
a policy (`adapter`, `device` or `guest`), with per-object overrides.
- **Identity.** Objects are identified by `psxw identity <game>`: placed
  text is matched against a pinned signature database, and the key is the
  sha256 of the text with relocated bytes masked.
- **Binding.** An adapter binds only to an object listed in sdk-psyq's
  `validated.rs` as (member, key). A function of any other object runs as
  guest code, cutting down to whatever lower functions do have adapters.
- **The build fails** if a function that itself touches I/O or kernel RAM
  is reachable from game code, through direct calls that never pass an
  adapter, with no validated adapter. Unreachable ones are listed in
  `build.json`.

### 6. Time, interrupts and blocking

Game-visible time is **virtual and deterministic**:
- **Spin loops.** Time advances when the guest blocks (`VSync(0)`,
  `CdReadSync(0)`, `DrawSync(0)`). It also advances by a fixed quantum
  whenever the guest's safepoint budget runs out. That is how a spin loop
  like `while (!cd_done);` makes progress.
- **Asynchronous events.** CD sector delivery, DMA completion, root counter
  ticks and vblank are all scheduled on the virtual timeline. They are
  delivered in a fixed order at safepoints, meaning runtime calls and loop
  back-edges.
- **Safepoint instrumentation.** A wasm pass adds a decrement-and-test of a
  global budget to every loop header in guest code.
- **Wall-clock time only paces presentation.** The same disc, build and
  input log always produce the same frames. That gives replays, golden-frame
  tests in CI, and bisection for free.

**Interrupts are the BIOS's exception handler, run at safepoints** (D45).
An IRQ becomes steps: every `SysEnqIntRP` element, priority 0 to 3, then
the exit. The exit is the custom one `HookEntryInt` installed if there is
one (libetc's), entered like a longjmp into the `setjmp` that made its
buffer. `ReturnFromException` leaves from any depth.
- **Translated code follows this** through wasm exceptions:
  - a `setjmp` (A0h:13h) caller saves its registers inline;
  - the return address is a continuation entry that reloads them;
  - B0h:17h throws;
  - the guest export `guest_invoke_irq` catches the throw and restores
    `$sp`/`$gp`.
- **So the SDK's own interrupt code runs unchanged.**

Drawing is synchronous inside the runtime: the GPU finishes "instantly".
Completion callbacks still arrive at the next safepoint, not re-entrantly
inside `DrawOTag`. CD timing is a runtime toggle, with a per-game default in
`game.toml`:
- *realistic*, because loading screens animate while reads are in flight;
- *instant*, an enhancement.

The game thread runs in a **Web Worker** and blocks with `Atomics.wait`,
which matches the PSX model exactly: the main loop blocks in `VSync`. JSPI
(suspending wasm on the main thread) is the fallback for environments
without `SharedArrayBuffer`. "Block" is a host trait, so the runtime doesn't
care which of the two is in use.

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
merged with the runtime (§7). The guest passes then run over guest functions only,
using walrus:
1. Address masking, including `memory.copy` and `memory.fill`.
2. The `__guest_sp` split.
3. Safepoints.
4. Rewriting indirect calls.

Finally `wasm-opt -O2` runs on the merged module.

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
                 │  (later: matching-build relocatable objects for the lift tier)
                 ▼
          psxw-inputs ──► ProjectModel (containers, AddressMap, overlay slots,
                 │                       per-function tier candidates)
                 │ per function: adapter │ decompiled C │ translated
     ┌───────────┴──────────┐            boundary manifest (adapters)
     ▼                      ▼                         │
 psxw-lift              decompiled C                  │
 MIPS .o → generated C       │                        │
     └──────────┬───────────┘                         │
                ▼                                     │
            psxw-cc: clang → wasm objects             │
                ▼                                     ▼
            psxw-link         boundary: adapter names from the runtime
                 │  place data/funcs at original addresses, dispatch table,
                 │  o32 adapters, overlay stubs, interior entries
                 ▼
            guest passes (mask, guest SP, safepoints) → guest.wasm
                 ▼
            wasm-merge ◄── prebuilt runtime.wasm (devices, BIOS, adapters)
                 ▼  then wasm-opt

            game.wasm  +  host (browser Worker shell, or wasmtime for tests)
```

## Runtime

The runtime is the PlayStation's operating system and devices,
reimplemented as a Rust library that the compiled game calls into. It never
steps anything. The game's own `main` loop is in charge, and game-visible
time moves only inside runtime calls (§6). Callbacks re-enter it: guest
calls runtime, runtime runs a guest callback, and the callback calls the
runtime again. So the runtime never holds a borrow of its own state across
a call into guest code.

- **Boot.** Does the BIOS's startup job: reads `SYSTEM.CNF` from the disc,
  loads the PS-X EXE into RAM, zeroes BSS, sets `__guest_sp`, and calls
  `guest_main`.
- **BIOS.** The A0/B0/C0 call tables and `syscall`: events (`OpenEvent`,
  `EnableEvent`, `TestEvent`, `WaitEvent`, `DeliverEvent`), the callback
  registry, critical sections, the heap, the file API on top of the CD,
  exception hooks, and `setjmp` support (guest `setjmp`/`longjmp` use wasm
  exception handling). The pure parts (heap, the string table) could come
  from OpenBIOS's C, compiled as guest code.
- **clock.** The virtual timeline: vblank (NTSC 59.94 Hz, PAL 50 Hz),
  timers, a queue of pending device events, and the safepoint quantum. It
  delivers events in a fixed order by calling guest callbacks.
- **gpu.** Split in two at a renderer command interface, so the PS1
  semantics stay host-agnostic:
  - **Front end, in the runtime.** Walks the ordering table in guest RAM,
    decodes packets, and tracks draw/display environments, texture pages and
    windows. It batches primitives and tracks VRAM hazards. It emits
    renderer commands: draw batch, VRAM upload, VRAM copy, VRAM readback,
    present.
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
- **spu.** 512 KB of SPU RAM, 24 voices with ADPCM, ADSR, pitch and noise,
  plus reverb and CD-input mixing. It renders one vblank's worth of samples
  per frame.
- **cd.** Disc images (`.bin`/`.cue`, `.iso`), the ISO9660 lookup that
  `CdSearchFile` needs, sector reads scheduled on the clock (with the
  timing toggle from §6), and XA-ADPCM streaming into the SPU's CD input.
- **sio.** The digital pad and DualShock buffers that `PadInitDirect` and
  `PadGetState` expect, and 128 KB memory card images.
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

The page's `window.wasmPsxScripting.execute(address, ...args)` is a
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
- **Adapters vs the translated original library.** Each adapter runs on the
  same inputs as the PsyQ function it replaces, and the device commands and
  SDK globals each produces are compared. A test-only decoder turns the
  original's register writes back into device commands; for the GPU that's
  free, since packets are the vocabulary. Every SDK version gets this
  conformance test automatically.
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
  - issue CD commands and record status and arrival times.

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
| CD timing | Device target: measured envelope; ours must fall inside it | Hardware only. Emulators approximate drive timing |
| Memory card | Exact by construction (format checksums; the game accepts its own saves) | Verified |
| Pad | Exact report formats; adapter vs the original libpad | Verified |
| BIOS | Device target running BIOS calls; a legitimately dumped BIOS image can also be translated and run as the oracle | Provisional |

Byte-exact GPU verdicts at 1× require exact rasterization in the WebGL2
backend: each primitive draws a covering quad, and the fragment shader runs
the PS1's edge and interpolation maths in integers. Upscaling and other
enhancements can't be checked against 1× hardware. They are layered on the
verified 1× mode as explicitly unverified options.

### Whole-game oracles

- **RAM by symbol against an emulator** (provisional). The original game
  runs in a headless emulator with the same inputs as psx-wasm. Because the
  memory layout is the original's, RAM can be compared by symbol at
  matching values of the game's own frame counter: "at frame 4,210,
  `gPlayerX` differs". `psxw oracle play` runs the emulator side and
  `psxw oracle ramdiff` the comparison; conventions in D39.
- **What the game can observe must be right.** VRAM readback, SPU key
  status, CD status, pad data and timer values feed back into game logic.
  Pixels on screen and audio samples are presentation. Timing has a range,
  not a value: CD seek times varied on real hardware and games were written
  to tolerate that, so realistic timing must fall inside the hardware's
  measured envelope.
- **The game checks itself.**
  - Deterministic replays (same disc, build and input log) give golden
    frame and audio hashes for regressions.
  - RAM invariants: no guest access into the I/O range, and the stack
    within bounds.
  - The game's own integrity checks, such as save checksums, pass.

## Pinning the inputs

Both decompilations are moving targets, so this repo pins exactly what it
consumes:
- **`inputs.lock`** records each input repo's commit SHA, the pinned clang
  distribution (URL and sha256), and the expected hashes of the
  user-supplied disc files. The disc files themselves are never committed.
- **`psxw inputs sync`** snapshots each repo at its pinned SHA with
  `git archive` into `.inputs/<name>-<sha>/`. The local checkout is used
  only as an object store: no worktree metadata is added to it, and its
  uncommitted work is never read (btn-decompilation had 147 modified paths
  on 2026-10-25).
- **Original bytes** come from the user's checkout at each splat config's
  `target_path`, accepted only if they match the config's `sha1`. Any
  splat-based decomp already declares both, so no repo-specific manifest is
  needed.
- **`psxw inputs bump`** moves a pin and prints the change in tiers ("37
  functions moved lift → c"), so each bump is a reviewable, testable event.
- **Matching builds only.** An input is accepted only if its matching build
  passes its own checksum gate (`make check-all` or `npm run verify`).

## Roadmap

The execution plan, one file per milestone, is in [`plans/`](../plans/README.md).
Each milestone's exit is gated by an automated oracle.

| # | Milestone | Gating oracle |
|---|---|---|
| M0 | Census | **Done** (see below) |
| M1 | Translator and linker core | **Done** (see below) |
| M2 | Whole-game link and headless boot | RAM equals the EXE image after boot; CD reads verify; overlays dispatch; main loop reached; deterministic |
| M3 | Oracle harness (Docker image: Beetle PSX + OpenBIOS; PS1 runner; vector recorder; golden store) | Exact-by-construction vectors reproduce through the emulator; deterministic captures |
| M4 | Pixels: GTE, GPU front end, WebGL2 backend (exact at 1×), pad, libgpu/libetc/libpad adapters | GTE and GPU vectors byte-exact against goldens; adapters match the original libraries; RAM by symbol through a replay |
| M5 | Browser host | Identical frame and RAM hashes to the native host for the same replay |
| M6 | Sound: SPU, XA, libspu adapters, translated libsnd | SPU and XA vectors against goldens; adapters match libspu; RAM including sound-driver symbols |
| M7 | Saves and a full play session | Card checksums; the game accepts its own saves; RAM by symbol over an in-game day |
| M8 | SDK generality: object-keyed adapters, layout generation, translated SDK cache, psyq-decomp C | Adapter conformance per object; psyq-decomp C matches translated |
| M9 | Second game | The same oracles pass with only a manifest |
| M10 | Hardware oracle (when a rig exists) | The vector archive replayed on a real console |
| M11 | Modkit: declared hooks on function boundaries; mods linked in, then loaded and hot-swapped as wasm | Example mods' effects by replay; an empty mod set builds the plain game; static and dynamic backends agree |
| M12 | Script mods: JavaScript hooks and commands on the moddable base, live in the browser; types from snapshots keyed by the game binary | **Done**: script twins equal their compiled mods bit for bit; a script pass-through on every hookable function equals the plain build |

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

  I/O and BIOS-kernel accesses all sit in SDK objects destined for adapters
  (`libpad/pdresres`, `libmcx/*`, `libcard/patch`, `libapi/first`).
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
| Game logic depends on CPU timing (root-counter measurements, busy-wait calibration) | The safepoint quantum is tunable per game. Such sites can be pinned to replaced or adjusted implementations. |
| Handwritten SDK asm uses non-ABI register conventions | Full-state lifting mode, chosen automatically when the lifter's analysis sees a boundary violation. |
| PSX RAM mirroring (`0x80200000` aliasing `0x80000000`) is relied on | The aux region occupies the mirrors. If a game needs mirroring, it can be enabled by masking with `0x1FFFFF` for that game. |
| Browser constraints: COOP/COEP, Gamepad API only on the main thread, Safari gaps | Worker plus SharedArrayBuffer as the primary design, JSPI as the fallback, and a desktop webview shell. |
| WebGL2 limits: no blending into integer textures, no framebuffer fetch | `R16UI` VRAM with all blending and mask logic in integer shader code; the destination is read from a read copy of VRAM; batches split on overlap. |
| An SDK object with no adapter (new version, patched library) | Translate it and cut one level lower, down to the BIOS calls; failing that, the build names the objects that need adapters. |

## Open questions

1. **"Webview" target.** Is the target a browser tab or PWA, or a desktop
   app in a webview shell such as Tauri? The answer affects COOP/COEP, file
   access and distribution.
2. **Export bundles.** Should the decompilation harness grow an `export`
   command that writes a versioned bundle (ELFs, objects,
   `compile_commands.json`, function inventory)? Otherwise `psxw` has to run
   each repo's build itself, which needs their vendored toolchains and their
   untracked inputs (`extracted/`, `tools/vendor/`). The bundle keeps the
   contract small, and pinning becomes a SHA plus a content hash.
3. **Distribution posture.** `game.wasm` contains compiled game code but
   no disc data. Will it be distributed, or only built locally from the
   user's own checkouts?
