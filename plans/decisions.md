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
  pinned Debian snapshot, on the oracle images' base, with no network. A
  test page runs on its own and POSTs its result to a small in-container
  server; a script opens each browser headless and waits for that file.
- **The web shell is plain ES modules,** with no build step.

*Why:* it reuses the pins the oracle images already have and needs no
Node toolchain. The image is much smaller than Playwright's, which
matters on this machine. The tests only need a page to load and report,
not to be scripted.

## Open questions (tracked)

- **Q1.** Webview shell (Tauri) or browser tab only. Decided in M5.
- **Q2.** Register-level SPU fallback for unknown libspu versions.
  Decided: yes (D41).
- **Q3.** The license for psx-wasm. Needed before any public release.
  Constraint so far: `psx-devices::sio` is a close Rust port of Beetle
  PSX's `frontio.c` (GPLv2), made so SIO0 matches the oracle bit for bit.
  A license that isn't GPL-compatible means re-implementing it from a
  behavioural description. Everything else is written from psx-spx, with
  the emulator used only as an oracle.
- **Q4.** The second game. Decided in M9.
