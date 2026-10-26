# psx-wasm: design sketch

Status: **sketch, 2026-10-25. Nothing is implemented.** The numbers in this
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
  `psx-gte`. When a function body contains real inline asm or a GNU nested
  function, `psxw-cc` blanks that body, so the function falls to the lift
  tier.
- **Lift tier: generated C.** The translator works one function at a time
  from *relocatable* objects: the per-TU `.o` files of the game's matching
  build, and the SDK's `.LIB`/`.a` members. It emits C, the approach
  N64Recomp and PSXRecomp use, rather than wasm directly:
  - **One pipeline.** Generated C goes through the same clang flags,
    prelude and guest memory model as decompiled C. The linker can't tell
    the two apart.
  - **Interchangeable with decompiled C.** Each function carries the
    prototype from its header, so decompiled C calls it directly and vice
    versa. When a decomp finishes a function, its readable C replaces the
    generated one.
  - **clang optimizes it.** Guest registers become locals that clang
    promotes to SSA form. `#line` directives point back at the original
    instructions for debugging.
  - **UB discipline.** The output uses wrapping unsigned arithmetic,
    explicit sign extension and masked shifts, so clang has no undefined
    behaviour to exploit.

  Relocations give the translator symbolic call targets, data references
  and jump tables. Delay slots are resolved statically, and load-delay
  hazards are modelled where a consumer sits in the slot. Other instructions
  are routed as follows:
  - COP2 maps onto `psx-gte`.
  - Jumps to the BIOS tables (`0xA0`/`0xB0`/`0xC0`, function number in
    `$t1`) and `syscall` become calls to the runtime's BIOS dispatcher.
  - COP0 and `break` become runtime calls.
  - A hardware register access is a build error: that code must have an
    adapter.

  The default is *ABI mode*, which assumes o32 at function boundaries.
  Clusters of handwritten asm that pass values in non-argument registers
  fall back to *full-state mode*, where every register lives in memory.
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
| libetc | Adapters for `VSync`, callbacks and interrupt setup. To be audited per member, like libapi. |
| libgpu | Adapters at the device edge: `ResetGraph`, `DrawOTag`, `DrawSync`, `LoadImage`, `StoreImage`, `MoveImage`, `ClearImage`, `PutDrawEnv`, `PutDispEnv`, `SetDispMask`. Primitive setup and OT helpers stay guest code. |
| libcd | Adapters at the device edge: `CdInit`, `CdControl*`, `CdRead*`, `CdSync`, `CdReady`, the callbacks, `CdSearchFile`. Helpers such as `CdIntToPos` stay guest code. |
| libspu | Adapters (register programming). Open question: register-level SPU emulation as the fallback for unknown versions. |
| libpad, libmcrd, libmcx, libcard | Adapters. |
| libsnd | Guest code. The sequencer *is* the game's music, so the original runs and only libspu beneath it is replaced. |
| libgs | Guest code. It only writes packets into RAM; `DrawOTag` is the device step. |
| libgte | Guest code, with COP2 through `psx-gte`. Hot wrappers may get adapters for speed. |
| libmath, libc | Guest code. `rand` must be the original, or the game plays differently. `memcpy` and `memset` may get adapters for speed. |

Adapters declare their C prototypes. The build checks them against the SDK
headers, so a signature drift fails at link time, not at run time. The
manifest is `boundary/psyq-470.toml` today. It becomes version-keyed, with
per-object overrides, when adapters are first written.

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

### 7. One linked module, with a precompiled runtime

- **The runtime is precompiled once per runtime version.** It is
  game-agnostic Rust, built as a wasm32 static library (relocatable
  objects). Its data, stack and heap sit above the guest window. It reads
  and writes guest memory through the window and never hands host pointers
  to the guest.
- **`psxw-link` links it with the game's cached objects** into one
  `game.wasm`. That puts the GTE math in the same module as the game's hot
  transform loops, where cross-module call overhead would hurt most.
- **The runtime is hot-swappable by relinking.** A new runtime archive means
  a relink of a few seconds, with no game code recompiled. `psxw-abi`
  carries an ABI version, and the linker refuses mismatched pairs.
- **Dev mode keeps them separate.** The runtime stays its own module sharing
  the same memory, so it can be swapped at page load without relinking. The
  ABI is the same; the linker just leaves the runtime imports unresolved.
- **The host interface is the only seam.** `game.wasm` imports a small host
  interface: frame pacing, audio output, input, disc reads, and the
  renderer command interface (see Runtime). The browser host implements it
  in the Web Worker. A native **wasmtime** host implements it for tests, CI,
  replays and profiling. Tests therefore run exactly the wasm that ships.
  The browser is the product, not the debugging environment.

### 8. psx-wasm owns the final link

Most of the job is *placement*: data at fixed addresses, function addresses
that are MIPS addresses, overlay stubs, and the split between guest and
runtime. wasm-ld can't express any of that.

`psxw-link` reads relocatable wasm objects as described in the
tool-conventions `Linking.md`, using `wasmparser` and `wasm-encoder`. Its
inputs are the game's objects, both decompiled and generated C, plus the
precompiled runtime archive. It resolves symbols against the address map
and emits the module. The guest passes then run over guest functions only,
using walrus:
1. Address masking, including `memory.copy` and `memory.fill`.
2. The `__guest_sp` split.
3. Safepoints.
4. Rewriting indirect calls.

Finally `wasm-opt -O3` runs.

Two alternatives are held in reserve:
- `wasm-ld --emit-relocs` followed by a re-homing pass, if writing the
  linker costs more than expected.
- An LLVM-IR rewriting pass, if masking overhead ever matters. LLVM could
  then hoist and CSE the masks.

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
            psxw-link ◄── precompiled runtime archive (devices, BIOS, adapters)
                 │  place data/funcs at original addresses, dispatch table,
                 │  o32 adapters, overlay stubs, interior entries
                 ▼
            guest passes (mask, guest SP, safepoints) → wasm-opt
                 ▼
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
    - **VRAM is one 1024×512 texture.** Primitives render into it, and the
      fragment shader samples textures from it, decoding 4/8-bit texels
      through their colour lookup tables itself. It has to be, because games
      render into VRAM and then texture from the result. It is stored as
      RGBA8, with the mask bit in alpha, because WebGL2 can't blend into
      integer textures.
    - **Feedback hazards.** A primitive that samples the area it draws into
      first gets a snapshot copy of that region.
    - **Semi-transparency** maps onto standard blend equations. Per-texel
      semi-transparency needs two passes, because WebGL2 has no dual-source
      blending. The mask bit uses the stencil buffer.
    - **The rest of the pixel pipeline is shader work:** affine texturing
      (`noperspective`), colour modulation, dithering, texture windows.
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

## Browser host

- **Worker.** Runs `game.wasm` and the host interface it imports, and owns
  the clock.
- **Presentation.** OffscreenCanvas inside the worker, drawing with the
  WebGL2 backend.
- **Audio.** The SPU writes each frame's samples into a SharedArrayBuffer
  ring, and an AudioWorklet drains it. The ring's fill level drives
  frame-pacing correction, so audio is the master clock.
- **Input.** The main thread polls the Gamepad API (it isn't available in
  workers) and keyboard state, and writes them into a SharedArrayBuffer.
- **Disc.** The user drops a `.bin`/`.cue` file, which is copied into OPFS.
  The worker reads it synchronously through `FileSystemSyncAccessHandle`.
  Memory cards are stored in OPFS too.
- **Cross-origin isolation.** `SharedArrayBuffer` requires COOP/COEP
  headers. A webview shell such as Tauri/wry sets them on its custom
  protocol; a static host needs them configured.

## Validation: sources of truth

Every check needs an oracle that we didn't write. The design keeps
emulators and test ROMs out of the verification path by being precise
about what the truth is for each layer.

**1. Above the device interface, the truth is the original code, and we
have all of it.** The matching decomp plus the original objects define the
behaviour of the game and every SDK library completely. These checks need
nothing external:
- **Translated vs decompiled C**, for every function that has both:
  thousands of functions across the two repos. This is the idea in
  btn-decompilation `plans/static-recompilation-project`.
- **Adapters vs the translated original library.** Each adapter runs on the
  same inputs as the translated PsyQ function it replaces, and the device
  commands and SDK globals each produces are compared. A test-only decoder
  turns the original's register writes back into device commands; for the
  GPU that's free, since packets are the vocabulary. Every SDK version gets
  this conformance test automatically.
- **Build-time invariants:**
  - each guest address has exactly one implementation;
  - C-defined data equals the original bytes;
  - struct layouts agree between GCC and clang (the M0 probe);
  - every cross-tier call agrees on its prototype;
  - resident-code fingerprints match the user's disc.
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

**2. At the device interface, the truth is the hardware's defined
behaviour.** For the parts that are exact arithmetic or fixed formats, the
specification is the oracle, written down from hardware research (psx-spx).
Spec-derived unit tests cover:
- GTE operations, including saturation and the FLAG register;
- ADPCM and XA decoding;
- CD sector formats and ISO9660;
- BIOS call semantics;
- packet decoding and VRAM transfer semantics;
- pad report and memory card formats.

**3. The game only needs what it can observe.** Anything that flows back
into guest RAM or return values must be right: VRAM readback, SPU key
status, CD status, pad data, timer values. Pixels on screen and audio
samples are presentation, which only the player sees.
- **Timing has a range, not a value.** The hardware itself varied: CD seek
  times differed from read to read. Games were written to tolerate that,
  so "realistic" CD timing needs to fall within the hardware's envelope,
  not match one console.
- **Exact rasterization and SPU interpolation are presentation details.**
  Real hardware is their only true reference. The target is "spec-faithful
  and looks right", and upscaling deliberately departs from exactness
  anyway. Where exactness matters, hardware captures are kept as data: test
  images and recordings, not programs to run.

**4. The game checks itself.**
- **Deterministic replays** (same disc, build and input log) give golden
  frame hashes and audio hashes for regressions.
- **RAM invariants:** no guest access into the I/O range, and the stack
  within bounds.
- **The game's own integrity checks**, such as save checksums, pass.
- **RAM is comparable by symbol.** The memory layout is the original's, so
  guest RAM can be diffed against any reference capture of the original by
  symbol name, if one ever exists. That reference is optional, not
  required.

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

Each milestone has an exit criterion. The riskiest assumptions are tested
first.

| # | Milestone | Exit criterion |
|---|---|---|
| M0 | **Census** (`psxw census <game>`). Pinned snapshots, the address map from splat configs and symbol files, and every game TU compiled to wasm objects. | Every TU compiles, with each normalization rule counted. Every function is placed exactly once, every reference resolves, and C-defined data matches the original bytes. **Layout probe:** GCC 2.95 and clang agree on the size, alignment and field offsets of every struct and global in every header the game includes. **Done for btn, 2026-10-25** (see below). |
| M1 | **Headless boot.** `psxw-link` (including the precompiled runtime archive), the guest passes, the BIOS dispatcher, a native wasmtime host, and every adapter stubbed (log and return 0). | `guest_main` runs from the runtime's boot to the main loop and through 600 vblanks without trapping. |
| M2 | **Lift tier.** The MIPS → generated C translator (ABI mode, jump tables, COP2 through `psx-gte`, BIOS jumps to the dispatcher). The translated SDK cache. | No traps on the path to gameplay. Translated matches decompiled C for every function that has both. |
| M3 | **Pixels.** The GPU front end, the WebGL2 backend at 1×, the GTE and the pad. | GTE and packet/VRAM-transfer spec tests pass. The title screen and first in-game frames are dumped as PNGs headless, with stable frame hashes across runs. |
| M4 | **Browser.** Worker, OPFS disc import, OffscreenCanvas, input. | Playable at full speed in Chrome and Firefox. |
| M5 | **Sound and saves.** SPU and libsnd, XA streaming, memory cards. | Play one in-game day with sound, save it, reload it. |
| M6 | **Second game.** Another decompilation through the same pipeline. | It runs with only a game manifest and boundary overrides. |

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
| WebGL2 limits: no blending into integer textures, no dual-source blending, no framebuffer fetch | VRAM stored as RGBA8 with the mask bit in alpha; two-pass per-texel semi-transparency; stencil for the mask bit. |
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
