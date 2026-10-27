# Decision log

Settled decisions, with their reasons. Each points to where
`docs/design.md` elaborates. To change one, record the proposal and
evidence in the current milestone's Findings and ask the human. Don't
silently diverge.

## Product and scope

**D1. What psx-wasm is.** A toolchain that turns a matching PlayStation
decompilation into a native WebAssembly build. It does no CPU emulation
and ships no register-level hardware model. It is a source port plus a
compatibility layer: high-level reproduction of the console's OS and
device behaviour, in the same family as libultraship, PsyCross and Wine.
*Why:* the human wants a truly native result, not an emulator.
(design: Goal; What "native, not an emulator" means)

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

## Memory and calls

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

## Implementations

**D8. One implementation per function.** In priority order:
1. an *adapter*, if the function touches hardware registers or BIOS
   internals;
2. else *decompiled C*;
3. else *translated*.

The rule is applied per function or member, never per library.
(design §4, §5)

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

**D13. libapi is translated.** 33 of the 39 members btn links are BIOS
trampolines, which the translator routes to the Rust BIOS. Adapters are
needed only for `counter.o` (it programs the timers) and for the members
that patch the Sony BIOS (`pad.o`, `patch.o`, `chclrpad.o`). (design §5)

**D14. Pure libraries run as guest code.** libsnd, libgs, libgte, libmath
and libc are never replaced for correctness; an adapter is allowed only
for speed, where equivalence is easy to show. *Why:* their behaviour is
what the game shows, plays and decides (`rand`, the music sequencer).
(design §5)

**D15. The decompiled SDK is not on the critical path.** Translation
covers everything. Decomp effort pays off most on the boundary libraries,
where it specifies the adapters.

## Runtime and host

**D16. The runtime is a precompiled Rust archive linked into
`game.wasm`.** Swapping it means a relink. Dev mode keeps it a separate
module sharing memory. The host interface is the only seam, and tests run
the exact wasm that ships. (design §7)

**D17. psx-wasm owns the final link.** wasm-ld can't express placement.
`wasm-ld --emit-relocs` plus a re-homing pass is the fallback. (design §8)

**D18. Time is virtual and deterministic.** It moves at blocking calls and
safepoints, and callbacks run at safepoints. The browser runs it in a Web
Worker paced with `Atomics.wait`; JSPI is the fallback. (design §6)

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

## Verification

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

## Inputs and process

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

## Open questions (tracked)

- **Q1.** Webview shell (Tauri) or browser tab only. Decided: browser
  tab only, for now (D46).
- **Q2.** Register-level SPU fallback for unknown libspu versions.
  Decided: yes (D41).
- **Q3.** The license for psx-wasm. Needed before any public release.
  Constraint so far: `psx-devices::sio` is a close Rust port of Beetle
  PSX's `frontio.c` (GPLv2), made so SIO0 matches the oracle bit for bit.
  A license that isn't GPL-compatible means re-implementing it from a
  behavioural description. Everything else is written from psx-spx, with
  the emulator used only as an oracle.
- **Q4.** The second game. Decided in M9.
