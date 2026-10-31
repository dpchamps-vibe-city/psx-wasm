# Shortcuts

Every place the runtime, the BIOS, the devices, the SDK adapters or the
translator does its job a simpler way than the spec, as found by an audit
on 2026-10-30. Each item is a task: redo it the spec's way, then tick it
with the commit.

## What counts

A **shortcut** is a corner cut: code that does the job, but skips a step,
condition or timing the spec has; folds or approximates where the spec
distinguishes; returns a default instead of modelling what happens;
reaches into another component's internals instead of its interface; or
relies on a heuristic that silently mishandles cases outside it.

Not listed here:
- the architecture: static recompilation, adapters in place of SDK code,
  register-level devices;
- features that aren't built yet;
- bugs, which are in [`open-items.md`](open-items.md) under *Bugs*;
- differences the architecture forces (see [Forced](#forced)).

## How to work an item

- **The spec decides what's right:** psx-spx, OpenBIOS's source, Beetle
  through the oracle, the original SDK object. Never what a current target
  game happens to need. Game runs are regression checks only. A run that
  changes after a fix points at a bug in the fix, or at code written
  against the old shortcut; fix that to the spec too.
- One item per commit where practical. Tick it and add the commit hash.
- If an item turns out to be forced by the architecture, move it to
  [Forced](#forced) with the reason.
- If a fix changes a recorded decision (a D-number), update
  `docs/design.md` in the same commit. Items whose Ref is a D-number are
  shortcuts taken on purpose; they still count.

Each item gives the location (paths relative to `crates/`), what the code
does, what the spec says, then: **Ref** (the decision or open item that
documents it, or *undocumented*), **Impact** (could a game other than the
current targets notice: low, med, high), **Checked** (*yes* when the claim
was re-read against the code after the audit, *audit* when it rests on the
audit's reading alone).

## Counts

| Area | Shortcuts | Documented | Undocumented |
|---|---|---|---|
| [BIOS](#bios) | 9 | 2 | 7 |
| [Emulated devices](#emulated-devices) | 13 | 7 | 6 |
| [SPU and renderer](#spu-and-renderer) | 4 | 1 | 3 |
| [SDK adapters](#sdk-adapters) | 16 | 14 | 2 |
| [Translator and runtime](#translator-and-runtime) | 16 | 7 | 9 |
| **Total** | **58** | **31** | **27** |

## Start here

By impact:
- TR4, overlay residency checked by two code words (high);
- TR5, virtual time counted in loop passes (high);
- TR1, lifted I/O accesses aren't volatile;
- TR6, every address masked to 24 bits;
- TR2, registers outside o32 arrive as 0;
- SDK1 and SDK2, SDK state kept outside guest RAM with no build check for
  other readers;
- BIOS4, the kernel's interrupt handlers run as Rust on device events;
- SDK3, SDK4 and SDK5, waits that don't spend the guest's budget (one
  shared helper);
- DEV1 and DEV2, CD command and seek times;
- DEV7, DEV10 and DEV12, drawing, MDEC and DMA take no time.

## BIOS

- [ ] **BIOS1. libapi's pad adapters write the BIOS's pad state.**
  `sdk-psyq/src/libapi.rs:95-109`. `InitPAD`, `StartPAD` and `PAD_init`
  set `m.kernel.pad_bufs`/`pad_started` instead of calling B0:12h, 13h
  and 15h. Ref: undocumented. Impact: low. Checked: yes.
- [ ] **BIOS2. libcard's wrappers never call the BIOS.**
  `sdk-psyq/src/libmc.rs:60-66`. `InitCARD`, `StartCARD` and `_bu_init`
  load the cards; `StopCARD` returns 0. B0:4Ah–4Ch and A0:55h are never
  reached. Ref: undocumented. Impact: low. Checked: yes.
- [ ] **BIOS3. libpad reads the BIOS's chain list.**
  `sdk-psyq/src/libpad.rs:183` checks `m.kernel.int_handlers` for its own
  element instead of its own state. Ref: undocumented. Impact: low.
  Checked: yes.
- [ ] **BIOS4. The kernel's interrupt handlers aren't chain elements.**
  `psx-bios/src/machine.rs:385-414`. Root counter events and pad buffer
  filling run in Rust when the device event fires, not as priority-ordered
  elements when the interrupt is taken. Ref: undocumented. Impact: med.
  Checked: yes.
- [ ] **BIOS5. The pad driver doesn't use SIO0.**
  `psx-bios/src/kernel.rs:110-126` writes host pad state straight into the
  buffers. Ref: undocumented. Impact: low-med. Checked: yes.
- [ ] **BIOS6. The memory card driver doesn't use SIO0.**
  `psx-bios/src/card.rs`. One operation per vblank; the pre-access check
  is synchronous. Ref: D43 (OpenBIOS's events). Impact: low. Checked: yes.
- [ ] **BIOS7. `SysEnqIntRP` chains live in Rust.**
  `psx-bios/src/io.rs:739-767`. Elements linked in RAM run after them; a
  game walking the chain in RAM doesn't see the kernel's.
  Ref: D65 (the RAM half). Impact: low. Checked: yes.
- [ ] **BIOS8. The event table lives in Rust, not kernel RAM.**
  `psx-bios/src/kernel.rs:37`. Ref: undocumented. Impact: low.
  Checked: yes.
- [ ] **BIOS9. CD file reads skip the CD controller.**
  `psx-bios/src/bios.rs:386-410`. Sectors come from the host instantly;
  no device table sits behind `open`. Ref: undocumented. Impact: low.
  Checked: yes.

## Emulated devices

### CD controller (`psx-devices/src/cdc.rs`, `cd.rs`)

- [ ] **DEV1. Every command's first response comes after a flat 1 ms.**
  `cd.rs:259-262`, used at `cdc.rs:365-426`. Second responses are fixed
  multiples (MotorOn/Stop ×4, Init ×2, ReadTOC ×8, GetID ×½). Spec: per
  command; spin-up and ReadTOC take far longer. Ref: D48 and open-items
  M10 task 4 (times); the flat ack is undocumented. Impact: med.
  Checked: audit.
- [ ] **DEV2. Seek time is a guessed formula.** `cd.rs:244-257`: 20 ms
  settle plus up to 300 ms of stroke. Ref: code comment; open-items M10
  task 4. Impact: med. Checked: audit.
- [ ] **DEV3. One sector buffer.** `cdc.rs:473-480`. An undelivered
  sector is replaced by the next. Ref: D66. Impact: low. Checked: audit.
- [ ] **DEV4. Bad and unreadable sectors are invisible.**
  `cdc.rs:461-467`. An EDC or header mismatch is only counted host-side and
  the sector is delivered; an unreadable LBA gives no interrupt and the
  read keeps scheduling. Spec: error or retry, INT5. Ref: undocumented.
  Impact: low. Checked: audit.
- [ ] **DEV5. A command written before the previous one's first response
  cancels that response.** `cdc.rs:298`. Hardware behaviour unverified.
  Ref: undocumented. Impact: low. Checked: audit.
- [ ] **DEV6. Reading past the data FIFO's end returns 0.**
  `cdc.rs:225-231`. Ref: undocumented. Impact: low. Checked: audit.

### GPU front end (`psx-devices/src/gpu.rs`)

- [ ] **DEV7. Drawing takes no time.** `gpu.rs:524-532, 353`. Bits 26 and
  28 are ready between whole commands; the FIFO is unbounded.
  Ref: D42, design §6. Impact: med. Checked: audit.
- [ ] **DEV8. GPUREAD past the rectangle returns 0.** `gpu.rs:551-553`.
  Spec: continues along the VRAM row. Ref: code comment. Impact: low.
  Checked: audit.
- [ ] **DEV9. The display's screen position is dropped.**
  `gpu.rs:660-684`. `present_desc` uses the display ranges only for width
  and height. Ref: undocumented. Impact: low (presentation).
  Checked: audit.

### MDEC (`psx-devices/src/mdec.rs`)

- [ ] **DEV10. Decoding is instant.** `mdec.rs:9-10, 160-190`.
  Ref: D58, D64. Impact: med (possibly open-items "FMV differences").
  Checked: audit.
- [ ] **DEV11. Reading an empty output FIFO returns 0.**
  `mdec.rs:192-196, 427-430`. Spec: DMA1 waits. Ref: D64. Impact: low-med.
  Checked: audit.

### DMA (`psx-devices/src/dma.rs`)

- [ ] **DEV12. Transfers complete instantly.** `dma.rs:3-5, 107-110`. No
  device request gating, no chopping (CHCR bit 8), DPCR priority bits
  ignored (ready channels run lowest number first). Ref: D42.
  Impact: med. Checked: audit.

### Memory card (`psx-devices/src/memcard.rs`)

- [ ] **DEV13. Writes commit instantly.** `memcard.rs:384-393`. No
  write-busy time between frames. Ref: undocumented. Impact: low.
  Checked: audit.

## SPU and renderer

- [ ] **SPU1. DMA4 ignores SPUCNT's transfer mode.**
  `psx-spu/src/lib.rs:400-413`, `psx-bios/src/io.rs:495-513`. Spec: DMA4
  proceeds only in DMA-read/-write mode. Ref: undocumented. Impact: low.
  Checked: audit.
- [ ] **SPU2. SPUSTAT bit 10 (transfer busy) is never set; bits 0–5
  mirror SPUCNT at once.** `psx-spu/src/lib.rs:290-315`. Spec: applied a
  bit delayed. Ref: undocumented. Impact: low. Checked: audit.
- [ ] **SPU3. SPUSTAT bit 11 only while RAMCNT = 0004h.**
  `psx-spu/src/lib.rs:311`. Fitted from two observed Beetle states.
  Ref: M06 Findings. Impact: low. Checked: audit.
- [ ] **SPU4. A failed output texture or framebuffer is swallowed.**
  `psx-render-gl/src/lib.rs:620-621`. The previous frame stays; other GL
  errors in the crate fail loudly. Ref: undocumented. Impact: low.
  Checked: audit.

## SDK adapters

### State kept outside guest RAM

- [ ] **SDK1. libpad's port records and buffers live in the runtime.**
  `sdk-psyq/src/libpad.rs:48-52, 146-161`. Unadapted code reading
  libpad's records in guest RAM sees stale values, and the build's chip
  decision (`psxw-build/src/game.rs:975-1037`) follows calls into code
  that touches hardware or kernel RAM, not reads of SDK globals.
  Ref: design §5 table; the module doc (`libpad.rs:48-50`): "only
  libpad's own code reads them" (`psxw sdk scan`, an SDK-level check).
  Impact: med. Checked: yes.
- [ ] **SDK2. libetc's `Vcount`, `Hcount` and handler, callback and DMA
  tables live in the runtime.** `sdk-psyq/src/libetc.rs:4-7, 125-158,
  303, 528-530`. Code reading them in guest RAM sees zeros; the build
  doesn't detect it (same check as SDK1). Ref: design §5 table; the module
  doc (`libetc.rs:4-7`): "no code but libetc's own reads them in either
  game (`psxw sdk scan`; the games' symbol files)", partly a per-game
  argument; open-items "Other difference classes ramdiff shows".
  Impact: med. Checked: yes.

### Waits that don't spend the guest's budget

- [ ] **SDK3. `VSync`'s wait.** `sdk-psyq/src/libetc.rs:556-563,
  572-585`. Advances the clock but never spends or restarts the guest's
  budget, so the next safepoint lands elsewhere. Ref: D66.
  Impact: low-med. Checked: audit.
- [ ] **SDK4. libgpu's `DrawSync(0)`, full-queue and `MoveImage2`
  waits.** `sdk-psyq/src/libgpu.rs:362-385, 395-401`. Count on a private
  phase. Ref: D66. Impact: low-med. Checked: audit.
- [ ] **SDK5. libpress's `DecDCTinSync`, `DecDCToutSync` and
  `DecDCTin`'s wait.** `sdk-psyq/src/libpress.rs:264-278`.
  Ref: undocumented (D66 names only `VSync` and libgpu). Impact: low.
  Checked: audit.

### Stand-ins for the original's own addresses and values

- [ ] **SDK6. `ResetCallback`, `StopCallback`, `RestartCallback` and
  `InterruptCallback(0/3)` return stand-in addresses.**
  `sdk-psyq/src/libetc.rs:69-75, 417, 436, 450`. Spec: `&intrEnv` and the
  real handlers. Ref: module doc. Impact: low. Checked: audit.
- [ ] **SDK7. Handlers and callbacks are called with a0–a3 = 0.**
  `sdk-psyq/src/libetc.rs:215-217`. Spec: I_STAT and DICR's address are
  left there. Ref: module doc. Impact: low. Checked: audit.
- [ ] **SDK8. `DMACallback(2, …)` returns a stand-in pump address.**
  `sdk-psyq/src/libgpu.rs:73-75`. Ref: module doc. Impact: low.
  Checked: audit.
- [ ] **SDK9. Messages go to the host log, not the game's `printf`.**
  `sdk-psyq/src/libgpu.rs:343, 869`; `libetc.rs:250, 578`;
  `libcd.rs:427, 638`. libpress and libspu already print through the
  game's `printf`. Ref: module docs (partly). Impact: low. Checked: audit.

### Bypassing a device's or the BIOS's interface

- [ ] **SDK10. libmcx's wire keeps its own SIO0 state and reads the
  BIOS's card images.** `sdk-psyq/src/libmc.rs:158-173, 269`. Ref: D65.
  Impact: low. Checked: audit.
- [ ] **SDK11. DMA1's busy flag is adapter state; CHCR1 is never busy.**
  `sdk-psyq/src/libpress.rs:33-34, 240-244, 406`. Ref: D64. Impact: low.
  Checked: audit.
- [ ] **SDK12. `DecDCTPutEnv` and transfers skip DMA bookkeeping.**
  `sdk-psyq/src/libpress.rs:42-46`. MADR/BCR not updated after a
  transfer, DPCR turning DMA1 off not modelled, the table copy's budget
  skipped. Ref: module doc. Impact: low. Checked: audit.
- [ ] **SDK13. `CD_datasync(0)` never waits.**
  `sdk-psyq/src/libcd.rs:659-665`. Returns 0 even with DMA3 busy.
  Ref: doc comment. Impact: low. Checked: yes.
- [ ] **SDK14. `CD_init` doesn't set the memory controller's delay
  registers.** `sdk-psyq/src/libcd.rs:46-48`. Ref: module doc.
  Impact: low. Checked: audit.
- [ ] **SDK15. `SetRCnt`, `GetRCnt` and `ResetRCnt` call the timer model,
  not its registers.** `sdk-psyq/src/libapi.rs:61-63, 70, 89-90`.
  Behaviour looks equivalent; `StartRCnt`/`StopRCnt` already use
  `set_reg`. Ref: undocumented. Impact: low. Checked: audit.
- [ ] **SDK16. `_spu_t(3)` writes the DMA controller's fields directly.**
  `sdk-psyq/src/libspu.rs:660-663`. The original writes CHCR4 with the
  start bit. Ref: module doc (loosely). Impact: low. Checked: audit.

## Translator and runtime

### psxw-lift

- [ ] **TR1. Lifted I/O accesses are plain memory to clang.**
  `psxw-lift/src/psxw_rt.h:23-33`, `emit.rs:416-437`. A load is volatile
  only when its result is unused, and I/O hooks are added after clang
  (`psxw-link/src/rewrite.rs:195-230`). Clang may drop the first of two
  stores to one register (CD parameters), merge two reads of one FIFO, or
  reorder accesses. Spec: every access happens, in order.
  Ref: undocumented (design §1 covers widening only). Impact: med.
  Checked: yes.
- [ ] **TR2. Registers passed outside o32 arrive as 0.**
  `psxw-lift/src/emit.rs:128-146`; `analysis.rs:16-17, 109-129, 326-327`;
  `lib.rs:291-297, 363-382`. Inlining evidence is block-local; s0–s7, fp
  and HI/LO are never evidence; C callers never inline. Should fail the
  lift instead. Ref: D33; open-items "Lifted SDK asm that reads
  temporaries on entry" (C callers only). Impact: med. Checked: audit.
- [ ] **TR3. A return address other than the caller's isn't detected.**
  `psxw-lift/src/graph.rs:345`, `emit.rs:138, 279-282, 323-324`.
  `la ra,L; j f`, an edited saved-ra slot, and a game's own
  setjmp/longjmp are silently wrong. Fail the lift when r31 can differ at
  a return or tail jump (the wasm return itself is forced).
  Ref: design §4 (the r31 = 0 idiom only). Impact: low-med.
  Checked: audit.

### psxw-link

- [ ] **TR4. Overlay residency is checked by two code words.**
  `psxw-link/src/game.rs:117, 838-868`; `psxw-build/src/game.rs:337-340,
  366-369, 423-429, 556-569`; `psxw-lift/src/program.rs:168-176`. The
  first candidate in name order wins; identical first words aren't
  checked; calls to addresses the main EXE also defines skip the stub;
  single-overlay addresses and intra-overlay calls aren't checked.
  Overlays sharing a slot often start with the same two instructions.
  Ref: D7, §3 (two-word compare, unchecked calls); ambiguity and
  main/overlay overlap undocumented. Impact: high. Checked: yes (the
  two-word compare).
- [ ] **TR5. Virtual time counts loop passes, not work.**
  `psxw-link/src/rewrite.rs:356-363`, `game.rs:689-708`;
  `psx-runtime/src/wasm.rs:335`, `driver.rs:144-151`;
  `psxw-cc/src/lib.rs:48`, `clang.rs:79, 95`. Straight-line code and
  recursion take no time; clang's unrolling and rotation change the count
  (no `-fno-unroll-loops`). Per-block instruction counts are feasible.
  Ref: §6, D18, D66; unrolling undocumented. Impact: high. Checked: audit.
- [ ] **TR6. Every address is masked to 24 bits.**
  `psxw-link/src/rewrite.rs:199-200, 212-213, 411-416`;
  `game.rs:780-783, 795-798`; `psxw-lift/src/analysis.rs:133-143`. RAM
  mirrors, EXP1, the BIOS ROM, KSEG2's cache control and scratchpad
  aliases fold onto RAM, zeros or the private page; a bad function pointer
  calls whatever sits at the folded index. Spec: RAM mirrored 4× in 8 MB;
  separate devices; bus errors elsewhere. Ref: design Risks (mirroring
  only). Impact: med. Checked: audit.
- [ ] **TR7. Variadic marshalling copies 8 words and doesn't realign
  64-bit arguments.** `psxw-link/src/marshal.rs:16, 96-101`.
  Ref: `marshal.rs:7-8` only. Impact: low-med. Checked: audit.
- [ ] **TR8. Writable data the address map doesn't name goes to AUX.**
  `psxw-link/src/layout.rs:147-167` (e.g. clang's `func.var` statics).
  Spec: at its original .data/.bss address. Ref: undocumented.
  Impact: low-med. Checked: audit.
- [ ] **TR9. `memory.copy`/`memory.fill` skip the I/O hook and the
  budget.** `psxw-link/src/rewrite.rs:304-328`; `game.rs:487-489`;
  `bodies.rs:89-109`. Ref: D36 (the expansion); the I/O bypass is
  undocumented. Impact: low. Checked: audit.
- [ ] **TR10. o32→C wrappers don't extend narrow parameters.**
  `psxw-link/src/marshal.rs:356, 512`. Ref: undocumented. Impact: low.
  Checked: audit.

### psxw-cc

- [ ] **TR11. `return;` in a non-void function isn't lifted.**
  `psxw-cc/src/compile.rs:141-145`, `clang.rs:31`. Only one diagnostic is
  matched, so it stays C and returns undef. Ref: undocumented (D35 covers
  falling off the end). Impact: low-med. Checked: audit.
- [ ] **TR12. `-funsigned-char` is hard-wired.** `psxw-cc/src/lib.rs:81`.
  Should come from the reference flags, as soft-float does
  (`psxw-census/src/lib.rs:58`). Ref: D27. Impact: low-med (non-PsyQ
  toolchains). Checked: audit.
- [ ] **TR13. Decompiled C loops get no compiler barrier.** Lifted loops
  do (`psxw-lift/src/emit.rs:167-173`). Clang can hoist a non-volatile
  load an interrupt changes out of a polling loop. Ref: undocumented.
  Impact: low. Checked: audit.
- [ ] **TR14. Soft-float detection works from a partial list.**
  `psxw-cc/src/compile.rs:381-392`. No unsigned i64 conversions,
  truncations, copysign or min/max. Ref: undocumented. Impact: low.
  Checked: audit.
- [ ] **TR15. GTE lowering drops `nop`s and ignores the load delay.**
  `psxw-cc/src/gte.rs:317-324`. Ref: undocumented. Impact: low.
  Checked: audit.

### psx-runtime and memory

- [ ] **TR16. Devices and the BIOS fold guest pointers with the same
  mask.** `psx-devices/src/mem.rs:4, 63-72`;
  `psx-runtime/src/wasm.rs:196-221`. Same aliasing as TR6 (DMA itself
  wraps correctly). Ref: undocumented. Impact: low. Checked: audit.

## Forced

Not tasks: these follow from having no CPU emulation, registers in wasm
locals, or static translation.

- Exceptions run as queued steps at safepoints; `ReturnFromException`
  unwinds instead of restoring registers (D45).
- `ChangeThread` saves only sp and gp into the TCB: the other registers
  are wasm locals in the caller (D57).
- The BIOS's own `setjmp` (A0:13h) returns only the first time; `longjmp`
  (A0:14h) traps.
- `Exec`/`LoadExec` of an executable that wasn't translated at build time.
- The clock moves only at safepoints; SIO0 and root counter 2 estimate the
  guest's cycle (`guest_now`, D66). A device register read sees the clock
  as of the last safepoint.
- An interrupt a device raises during a register access is taken at the
  guest's next safepoint (D64).
- Device accesses and DMA cost the CPU nothing: no wait states, no stall.
- `jr ra` is a wasm return and r31 starts at 0 (design §4).
- A `void` C function answers v0 = 0, an `int` one v1 = 0
  (`psxw-link/src/marshal.rs:189-197`; open-items Bugs).
- Uninitialised locals read 0 and clang's stack layout differs from GCC's
  (`-ftrivial-auto-var-init=zero`, D27).

## Not shortcuts

Listed so they aren't re-added:
- "A DPCR write leaves a second ready channel pending": wrong;
  `run_dma` (`psx-bios/src/io.rs:353-359`) runs every ready channel.
- XA reserved shifts 13–15 and Table5 taps 24/26: the references disagree
  (open-items M10).
- The `Instant` CD timing mode: an opt-in option (D19).
- The XA resampler's fixed starting phase: a determinism choice.
- Reading SPU DATA returns 0: the register is write-only, and this
  matches the oracle (M06).
- GTE undefined opcodes: undefined on hardware, matching Beetle
  (open-items).

## Progress log

- 2026-10-30: list created from the audit of `psx-bios`, `psx-devices`,
  `psx-spu`, `psx-gte`, `psx-render-gl`, `sdk-psyq`, `psxw-lift`,
  `psxw-link`, `psxw-cc` and `psx-runtime` (`3eff117`, 121 items).
  Narrowed to shortcuts only: bugs moved to open-items *Bugs*; features
  not yet built dropped (still in `3eff117`).
