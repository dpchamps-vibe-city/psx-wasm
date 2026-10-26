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
| `0x0200_0000`– | Data, stack and heap of `runtime.wasm`, set with `--global-base`. |

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

Each overlay's functions are compiled into `guest.wasm` alongside the EXE.
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

### 4. Every function gets one tier

Each function at each guest address is implemented exactly once. `psxw`
chooses the implementation per build:

| Tier | Source | When it's used |
|---|---|---|
| `c` | Decompiled C, compiled by clang | The default when C exists and compiles cleanly under the host prelude. |
| `replace` | A Rust runtime import | The boundary manifest says so. These are the hardware edge, chosen per SDK version and per function. |
| `lift` | Original MIPS from the matching build's relocatable objects, translated ahead of time | `INCLUDE_ASM`, functions with real inline asm, and SDK members that have no C yet. |

How each tier is produced:

- **C tier.** A prelude, injected with `-include`, neutralizes MIPS-isms.
  It strips register-pinning asm. It redefines `INCLUDE_ASM(dir, f)` to emit
  a lift marker. It maps the `gte_*` macros onto `psx-gte`. When a function
  body contains real inline asm, `psxw-cc` blanks that body, so the function
  falls to the lift tier.
- **Lift tier.** The lifter works one function at a time, from *relocatable*
  objects:
  - The per-TU `.o` files of the game's matching build.
  - The SDK `.a` members.

  Relocations give it symbolic call targets, data references and jump
  tables. Delay slots are resolved statically. Load-delay hazards are
  modelled where a consumer sits in the slot. COP2 instructions map onto
  `psx-gte`. COP0, `syscall` and `break` become runtime calls.

  The default is *ABI mode*: the lifter assumes o32 at function boundaries,
  so registers become wasm locals. Clusters of handwritten asm that pass
  values in non-argument registers fall back to *full-state mode*, where
  every register lives in memory.
- **The tier report.** `psxw report tiers` lists every function's tier and
  how it changed since the last lock. At run time, the profiler reports the
  share of executed code that comes from each tier. The lift tier is a
  migration aid that shrinks as the decompilations progress. The lifted
  share of executed code is a number to drive toward zero.

### 5. The SDK boundary sits at the hardware edge

The game's matching link fixes the set of SDK functions. Each one is
compiled or lifted like game code *unless* the boundary manifest
(`boundary/psyq-470.toml`) replaces it. The manifest belongs to the SDK
version, not the game, so the next PsyQ 4.7 game reuses it. Games can still
override entries.

The rule is to replace the lowest function that has a clean semantic
contract, and run everything above it as the game's own code:

| Library | Policy |
|---|---|
| libapi | Replaced. These are BIOS A0/B0/C0 stubs: events, heap, file I/O, critical sections and `FlushCache`. |
| libetc | Replaced. This covers `VSync`, the callbacks, root counters and interrupt setup. |
| libgpu | Replaced at the device edge: `ResetGraph`, `DrawOTag`, `DrawSync`, `LoadImage`, `StoreImage`, `MoveImage`, `ClearImage`, `PutDrawEnv`, `PutDispEnv` and `SetDispMask`. Primitive setup, OT helpers and `GetTPage`-style helpers stay guest code. |
| libgs | Guest code. It only builds packets on top of libgpu and libgte. |
| libgte | Lifted or C by default, using `psx-gte` for COP2. Hot entry points can be replaced in Rust for speed. |
| libcd | Replaced at the device edge: `CdInit`, `CdControl*`, `CdRead*`, `CdSync`, `CdReady`, the callbacks and `CdSearchFile`. Helpers such as `CdIntToPos` stay guest code. |
| libspu | Replaced, because it is register programming. |
| libsnd | Guest code: the SEQ/VAB sequencer runs on top of the replaced libspu, ticked by the runtime's timer callbacks. |
| libpad, libmcrd, libmcx, libcard | Replaced. |
| libc, libmath | Guest code. A few hot functions (`memcpy`, `memset`) can be replaced for speed. |

Rust replacements declare their C prototypes. The build checks those
prototypes against the SDK headers, so a signature drift fails at link
time, not at run time.

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
inside `DrawOTag`. CD timing is configurable:
- *realistic*, the default, because loading screens animate while reads are
  in flight;
- *instant*, an enhancement.

The game thread runs in a **Web Worker** and blocks with `Atomics.wait`,
which matches the PSX model exactly: the main loop blocks in `VSync`. JSPI
(suspending wasm on the main thread) is the fallback for environments
without `SharedArrayBuffer`. "Block" is a host trait, so the runtime doesn't
care which of the two is in use.

### 7. Two modules sharing one memory

- **`guest.wasm`** is per game and built by `psxw-link`. It holds the game,
  the SDK's C, the lifted code, adapters and dispatch stubs. It imports
  memory and the `psx.*` runtime functions, and exports `guest_main` and
  `guest_invoke(addr, a0..a3) -> i64`. The runtime uses `guest_invoke` to
  run guest callbacks.
- **`runtime.wasm`** is game-agnostic Rust, built with `--import-memory
  --global-base=0x02000000`. It reads and writes guest memory through the
  window and never hands host pointers to the guest.

Calls in either direction are wasm-to-wasm import calls, so no JavaScript
sits on the hot path. The same `psx-runtime` crate is also built natively
and embedded with `guest.wasm` in **wasmtime**. That headless host is what
runs tests, CI, replays and profiling. The browser is the product, not the
debugging environment.

### 8. psx-wasm owns the final link

Most of the job is *placement*: data at fixed addresses, function addresses
that are MIPS addresses, overlay stubs, the split between guest and host,
and lifted functions spliced in. wasm-ld can't express any of that.

`psxw-link` reads relocatable wasm objects as described in the
tool-conventions `Linking.md`, using `wasmparser` and `wasm-encoder`. It
resolves symbols against the address map and emits the module. The guest
passes follow, using walrus:
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
     ┌───────────┼──────────────────────────┐
     ▼           ▼                          ▼
 psxw-cc     psxw-lift                 boundary manifest
 clang →     MIPS .o →                 (replace → psx.* imports)
 wasm .o     wasm functions
     └───────────┬──────────────────────────┘
                 ▼
            psxw-link ── place data/funcs at original addresses, verify data
                 │       image vs original, adapters, overlay stubs, table
                 ▼
            guest passes (mask, guest SP, safepoints) → wasm-opt
                 ▼
            guest.wasm  +  runtime.wasm (game-agnostic)  +  web shell
```

## Runtime (`psx-runtime`, host-agnostic)

- **kernel.** Events (`OpenEvent`, `EnableEvent`, `TestEvent`,
  `WaitEvent`, `DeliverEvent`), the callback registry, critical sections,
  heap (`InitHeap`), the BIOS file API on top of the CD, and `setjmp`
  support. Guest `setjmp` and `longjmp` use wasm exception handling.
- **clock.** The virtual timeline, vblank (NTSC 59.94 Hz, PAL 50 Hz), root
  counters and the safepoint quantum.
- **gpu.** A 1024×512×16bpp VRAM model and a software rasterizer that
  covers all primitive types, texture modes, CLUTs, semi-transparency,
  dithering, mask bit and texture windows. It also does the VRAM transfers,
  and turns the display area into an RGBA frame.
  - Software rendering comes first because the game reads VRAM back
    (`StoreImage`, 8 call sites) and copies within it (`MoveImage`, 13 call
    sites), and that needs exact pixels.
  - A wgpu hardware renderer with upscaling is a later, optional backend.
- **gte.** Re-exported from `psx-gte`. Its register file lives in the
  runtime-private page of the guest window, so C macros, lifted COP2 code
  and Rust libgte replacements all see the same state.
- **spu.** 512 KB of SPU RAM, 24 voices with ADPCM, ADSR, pitch and noise,
  plus reverb and CD-input mixing. It renders one vblank's worth of samples
  per frame.
- **cd.** Disc images (`.bin`/`.cue`, `.iso`), the ISO9660 lookup that
  `CdSearchFile` needs, sector reads scheduled on the clock, and XA-ADPCM
  streaming into the SPU's CD input.
- **pad / memcard.** The digital pad and DualShock buffers that
  `PadInitDirect` and `PadGetState` expect, and 128 KB card images.

## Browser host

- **Worker.** Runs `runtime.wasm` and `guest.wasm` and owns the clock.
- **Presentation.** OffscreenCanvas inside the worker, drawing with WebGPU,
  or WebGL2 where WebGPU isn't available.
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

## Validation

1. **Build-time invariants**
   - Each guest address has exactly one implementation.
   - The data image equals the original bytes.
   - Every cross-tier call agrees on its prototype.
   - The resident-code fingerprints match the user's disc.
2. **Lifter conformance**
   - Instruction-level tests against a reference interpreter that exists
     only in the test suite and never ships.
   - Function-level differential tests of **lifted vs C** for every
     function that has both. That covers thousands of functions across the
     two repos, and the matching decompilations are the oracle (this is the
     idea in btn-decompilation `plans/static-recompilation-project`).
3. **The C tier under clang**
   - Matched C is tuned to GCC 2.95 and can rely on undefined behaviour:
     signed overflow, uninitialized locals used to steer register
     allocation, type punning, and out-of-bounds indexing into neighbouring
     fields.
   - Compile with `-std=gnu89 -fwrapv -fno-strict-aliasing
     -fno-delete-null-pointer-checks -funsigned-char -ffreestanding
     -ftrivial-auto-var-init=zero`.
   - Test builds add `-fsanitize=undefined` with trap handlers that the
     runtime implements.
   - Differential tests of C vs lifted catch whatever the flags miss.
4. **Services**
   - Run Amidog's `psxtest_gte` against `psx-gte`.
   - Run the ps1-tests GPU and SPU suites against the rasterizer and the
     mixer.
5. **The whole game.** Deterministic input replays run under wasmtime in
   CI and are checked against golden frame hashes. Key frames are compared
   against emulator screenshots, with a tolerance.

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
| M1 | **Headless boot.** `psxw-link`, the guest passes, a native wasmtime host, and every `replace` function stubbed (log and return 0). | `guest_main` runs from the runtime's boot to the main loop and through 600 vblanks without trapping. |
| M2 | **Lift tier.** ABI-mode lifter, jump tables, COP2 through `psx-gte`. | No traps on the path to gameplay. Lifted matches C for every function that has both. |
| M3 | **Pixels.** The GPU service, software rasterizer, GTE and pad. | The title screen and first in-game frames are dumped as PNGs headless, and their frame hashes are stable across runs. |
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
3. **Distribution posture.** `guest.wasm` contains compiled game code but
   no disc data. Will it be distributed, or only built locally from the
   user's own checkouts?
