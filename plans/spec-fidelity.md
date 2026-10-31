# Spec fidelity

**Status:** not started.
**Depends on:** [unified adapter layer, phase 1](unified-adaptor-layer/phase1.md)
(done): the adapters this plan corrects.
**Replaces:** `plans/shortcuts.md` (the 2026-10-30 audit, commits
`3eff117`–`0bae88a`) and the audit's bugs in `open-items.md`.

## Goal

The BIOS, the SDK adapters, the renderer, the translator and the runtime
do their jobs the spec's way:
- every shortcut the 2026-10-30 audit found is gone (42);
- every bug it found is fixed (23);
- the BIOS is implemented properly: every A0h/B0h/C0h function and
  syscall psx-spx documents behaves as the spec says, with its state where
  the kernel keeps it, or fails with the reason it can't.

## Why

A shortcut is a corner cut: code that does the job, but skips a step,
condition or timing the spec has; folds or approximates where the spec
distinguishes; returns a default instead of modelling what happens;
reaches into another component's internals instead of its interface; or
relies on a heuristic that silently mishandles cases outside it. The
current targets mostly don't notice. The next game can, and silently.

## Scope

- **In:** `psx-bios`, `sdk-psyq`, `psx-render-gl`, `psxw-lift`,
  `psxw-link`, `psxw-cc`, `psx-runtime`.
- **Out:** the emulated devices' own fidelity (`psx-devices`, `psx-spu`).
  They are the fallback while the adapters are completed
  ([`unified-adaptor-layer/`](unified-adaptor-layer/)). The six device
  bugs the audit found are in phase 1 only because each is a line or two
  (B10–B15).
- **Not tasks:** differences the architecture forces (see
  [Forced](#forced)).

## Rules

- **The spec decides what's right:** psx-spx, OpenBIOS's source, Beetle
  through the oracle, the original SDK object (`psxw sdk disasm`,
  psyq-decomp). Never what a current target game happens to need.
- **Game runs are regression checks only.** A run that changes after a fix
  points at a bug in the fix, or at code written against the old shortcut;
  fix that to the spec too. Re-record a replay or baseline only once the
  new behaviour is shown to match the spec or the oracle, and say so in
  the progress log.
- **The human runs the games.** `psxw ci`, replays and other long runs are
  theirs (plans/README.md, "How to execute this plan"). The agent builds,
  then hands over the exact commands. Each phase's exit criteria mark
  those runs.
- **Decisions first.** A phase that changes a recorded decision starts by
  asking the human, then records it in `docs/design.md` (body and decision
  log) before any code.
- **Locations come from the audit.** Re-read the code before changing it;
  line numbers drift.
- **Tests:** mostly integration. A fix that changes observable behaviour
  gets a test in the existing test module for that area; don't build new
  scaffolding for one item.
- **One item per commit** where practical. Tick it with the hash.
- **An item that turns out forced** moves to [Forced](#forced) with the
  reason. An item that turns out wrong is struck through with the reason.

Paths are relative to `crates/`.

## Decisions this plan needs

| Phase | Decision | Revises |
|---|---|---|
| 2 | SDK state stays where the SDK keeps it: libetc's and libpad's adapters keep their state in the objects' own data, found through their code (D61's way). | design §5's "their state in the runtime" for libetc and libpad |
| 3 | The kernel keeps its state in kernel RAM as the BIOS does (call tables, event control blocks, interrupt chains); its own interrupt handlers are chain elements; its pad, card and file drivers drive the devices. | extends D57 and D65; revises D43's card driver |
| 5 | Overlay residency comes from what was loaded, not from code words. | D7 |
| 6 | The memory map is the console's: mirrors, regions, bus errors. | D5 |
| 7 | Virtual time comes from the original's instruction counts, not loop passes. | §6, D18, D66 |

## Phases

| Phase | What | Items | Runs needed |
|---|---|---|---|
| [1](#phase-1-bugs-and-small-shortcuts) | Bugs and small shortcuts | 39 | one batch at the end |
| [2](#phase-2-sdk-state-and-waits) | SDK state and waits | 8 | after tasks 3, 5 and 6 |
| [3](#phase-3-the-bios) | The BIOS, properly | 10 tasks | after tasks 3–6 |
| [4](#phase-4-translator-correctness) | Translator correctness | 7 | one batch |
| [5](#phase-5-overlay-residency) | Overlay residency | 1 | one batch |
| [6](#phase-6-the-memory-map) | The memory map | 2 | one batch |
| [7](#phase-7-time) | Time | 1 | re-record baselines |

Phases 1–4 can overlap where they touch different crates. Phases 5–7 each
change a core decision; 7 goes last because it moves every game's timing,
so baselines are re-recorded once.

## Phase 1: bugs and small shortcuts

Nothing here needs a design decision.

### Bugs

BIOS:
- [ ] **B1. `SetDefaultExitFromException` (B0:18h) does nothing.**
  `psx-bios/src/bios.rs:339`. The exception handler keeps entering the old
  custom exit (`psx-runtime/src/driver.rs:51, 85`) after libetc's
  `StopCallback`. Fix: clear `kernel.custom_exit`.
- [ ] **B2. `EnableEvent`/`DisableEvent` change closed event slots.**
  `psx-bios/src/bios.rs:303-316`: enabling a closed handle revives it;
  disabling one leaks it (`OpenEvent` reuses only unused slots,
  `kernel.rs:70`). Fix: change only slots in use.
- [ ] **B3. Memory card sector 1024 is accepted as 1023.**
  `psx-bios/src/card.rs:178, 186`. Fix: `>=`, and drop the clamp.
- [ ] **B4. B0:15h and libapi's `PAD_init` disagree on their arguments.**
  `psx-bios/src/bios.rs:332` vs `sdk-psyq/src/libapi.rs:105`. Fix: settle
  against psx-spx (`OutdatedPadInitAndStart(type, button_dest, …)`) and
  the original `pad.o`; make both agree (BIOS1 then routes `PAD_init`
  through B0:15h).
- [ ] **B5. `open` on `cdrom:` hands out descriptor 16 when all are
  taken.** `psx-bios/src/bios.rs:362`. Fix: fail as the card path does.
- [ ] **B6. `bios::implemented()` counts calls that always error**
  (`longjmp`, `Exec`, `ReturnFromException`, `SystemError`), which the
  build's coverage check trusts. `psx-bios/src/bios.rs:104`. Fix: count
  only calls that can succeed.
- [ ] **B7. BIOS `printf` zero-pads negatives as `000-5`** and ignores
  `+`, space and `#`. `psx-bios/src/bios.rs:597-607`. Fix: sign before
  padding; the flags.
- [ ] **B8. Stale doc on `card_queue`.** `psx-bios/src/card.rs:162`: it
  always returns true.
- [ ] **B9. The vblank counter event ignores I_MASK.**
  `psx-bios/src/machine.rs:392`; counters 0–2 are gated
  (`machine.rs:410`), and libapi's `StartRCnt(RCntCNT3)` sets bit 0
  expecting it. Fix: gate on bit 0 (phase 3 task 5 later replaces both
  with the kernel's handler).

Devices (out of scope otherwise):
- [ ] **B10. `GetlocP` loses a minute.** `psx-devices/src/cdc.rs:389-397`:
  the pregap is subtracted from BCD seconds with no borrow (00:58 reads
  01:00); index fixed at 1; reports the head, not the current sector.
  Fix: work in LBAs, convert once.
- [ ] **B11. GPUSTAT bits 31 and 13 follow a per-vblank field.**
  `psx-devices/src/gpu.rs:516-517`. Fix: psx-spx (bit 31 per scanline in
  240-line modes, 0 in vblank; bit 13 is 1 when not interlaced).
- [ ] **B12. GPUSTAT bit 25 is wrong for DMA directions 1–3.**
  `psx-devices/src/gpu.rs:518-520`. Fix: 1 = FIFO not full, 2 = bit 28,
  3 = bit 27.
- [ ] **B13. Timer 0's dot clock runs 4–10× too fast.**
  `psx-devices/src/timers.rs:31`, `clock.rs:56-58`. Fix: divide the video
  clock by the horizontal resolution's 10/8/7/5/4.
- [ ] **B14. Muting or disabling the SPU silences CD audio.**
  `psx-spu/src/lib.rs:722-724`. Fix: SPUCNT bits 14–15 don't affect the CD
  input (psx-spx).
- [ ] **B15. SPU manual-transfer writes past 32 are dropped.**
  `psx-spu/src/lib.rs:349-354`. Fix: settle with the oracle what overflow
  and late DATA writes do.

Renderer, SDK, translator, runtime:
- [ ] **B16. 15-bit colour is shifted, not expanded.**
  `psx-render-gl/src/shaders.rs:341-344` (white shows as F8F8F8). Fix:
  `(c << 3) | (c >> 2)`.
- [ ] **B17. libgpu's timeout counts every vblank** when libetc's adapters
  aren't running. `sdk-psyq/src/libgpu.rs:324-328`. Fix: read `Vcount` as
  the original does (`VSync(-1)`).
- [ ] **B18. libmcx's last-byte wait is ~20 cycles, not a byte's ~1,000.**
  `sdk-psyq/src/libmc.rs:498-505`; probably a D66 regression. Fix: a
  byte's time at the port's baud rate.
- [ ] **B19. A tail transfer out of an inlined callee exits the whole
  caller.** `psxw-lift/src/graph.rs:231-237, 260-264`; `emit.rs:198-201,
  255-258, 306-314`. Fix: return to the inlined call's return site.
- [ ] **B20. `bltzal`/`bgezal` with rs = ra test the new link value.**
  `psxw-lift/src/emit.rs:213-234`. Fix: capture the condition before
  writing r31.
- [ ] **B21. `psxw_lift::bios_calls()` recognises only `addiu t1`.**
  `psxw-lift/src/lib.rs:524-528`; an `ori t1` ChangeThread trampoline
  misses the Asyncify scope (`psxw-build/src/game.rs:527`). Fix: use
  `t1_after_slot` (`lib.rs:51`).
- [ ] **B22. GTE operands parse shifts tighter than `+`.**
  `psxw-cc/src/gte.rs:236-259`. Fix: C's precedence.
- [ ] **B23. The EXE header's stack beats `SYSTEM.CNF`'s `STACK=`.**
  `psx-runtime/src/boot.rs:68`. Fix: check psx-spx's `LoadExec` first; if
  it writes `STACK` into the header, `STACK` wins.

### Small shortcuts

- [ ] **BIOS1. libapi's pad adapters write the BIOS's pad state.**
  `sdk-psyq/src/libapi.rs:95-109`. Fix: call B0:12h, 13h and 15h.
- [ ] **BIOS2. libcard's wrappers never call the BIOS.**
  `sdk-psyq/src/libmc.rs:60-66`. Fix: call B0:4Ah–4Ch, A0:55h/70h and
  return what they return.
- [ ] **BIOS3. libpad reads the BIOS's chain list** to see whether its
  element is installed. `sdk-psyq/src/libpad.rs:183`. Fix: keep that in
  libpad's own state (`PadStartCom`, `PadStopCom`).
- [ ] **SDK7. libetc calls handlers and callbacks with a0–a3 = 0.**
  `sdk-psyq/src/libetc.rs:215-217`. Fix: pass what the original leaves
  (I_STAT; DICR's address).
- [ ] **SDK9. Messages go to the host log, not the game's `printf`.**
  `sdk-psyq/src/libgpu.rs:343, 869`; `libetc.rs:250, 578`;
  `libcd.rs:427, 638`. Fix: print through the game's `printf`, as
  libpress and libspu do.
- [ ] **SDK12. `DecDCTPutEnv` and transfers skip DMA bookkeeping.**
  `sdk-psyq/src/libpress.rs:42-46`. Fix: MADR/BCR after a transfer,
  DPCR's DMA1 enable, the table copy's passes.
- [ ] **SDK13. `CD_datasync(0)` never waits.**
  `sdk-psyq/src/libcd.rs:659-665`. Fix: wait while DMA3 is busy, with the
  original's timeout.
- [ ] **SDK14. `CD_init` doesn't set the memory controller's delay
  registers.** `sdk-psyq/src/libcd.rs:46-48`. Fix: write them as the
  original does.
- [ ] **SDK15. `SetRCnt`, `GetRCnt`, `ResetRCnt` call the timer model.**
  `sdk-psyq/src/libapi.rs:61-63, 70, 89-90`. Fix: the counter registers
  through `set_reg`, as `StartRCnt` does.
- [ ] **SDK16. `_spu_t(3)` writes the DMA fields directly.**
  `sdk-psyq/src/libspu.rs:660-663`. Fix: MADR, BCR and CHCR4 through the
  register path.
- [ ] **REN1. A failed output texture or framebuffer is swallowed.**
  `psx-render-gl/src/lib.rs:620-621`. Fix: fail loudly, as the crate does
  elsewhere.
- [ ] **TR1. Lifted I/O accesses are plain memory to clang.**
  `psxw-lift/src/psxw_rt.h:23-33`, `emit.rs:416-437`; the I/O hooks come
  after clang (`psxw-link/src/rewrite.rs:195-230`). Clang may drop,
  merge or reorder register accesses. Fix: volatile for every access the
  lifter can't prove is plain RAM (constant I/O addresses, and every access
  in device-tier SDK objects).
- [ ] **TR10. o32→C wrappers don't extend narrow parameters.**
  `psxw-link/src/marshal.rs:356, 512`. Fix: sign- or zero-extend from
  the known kinds.
- [ ] **TR11. `return;` in a non-void function isn't lifted.**
  `psxw-cc/src/compile.rs:141-145`, `clang.rs:31`. Fix: match both of
  clang's return-type diagnostics and lift as for falling off the end
  (D35).
- [ ] **TR12. `-funsigned-char` is hard-wired.** `psxw-cc/src/lib.rs:81`.
  Fix: from the reference cc1 flags, as soft-float is
  (`psxw-census/src/lib.rs:58`).
- [ ] **TR14. Soft-float detection works from a partial list.**
  `psxw-cc/src/compile.rs:381-392`. Fix: every wasm float operation.

### Exit criteria

1. `cargo test --workspace` passes.
2. `psxw build btn` and `psxw build bof4` succeed.
3. `psxw oracle check btn --device bios` passes (verdicts provisional
   against OpenBIOS).
4. **Human:** `psxw ci btn` and `psxw ci bof4` pass.

## Phase 2: SDK state and waits

1. [ ] **Record the decision** (table above, phase 2).
2. [ ] **SDK2. libetc's state into its own data.** `Vcount`, `Hcount`,
   the handler, callback and DMA tables, the dispatcher's flags:
   `sdk-psyq/src/libetc.rs:4-7, 125-158, 303, 528-530`. Find them through
   the validated objects' code, as libgpu's adapters do (D61). The
   adapters read and write them there; nothing of libetc's stays in
   `m.sdk.etc` that the original keeps in RAM.
3. [ ] **SDK1. libpad's state into its own data.** Port records, receive
   buffers, pending results: `sdk-psyq/src/libpad.rs:48-52, 146-161`.
   Same way as task 2. **Run:** both games' replays.
4. [ ] **SDK6, SDK8. The real addresses.** `ResetCallback`,
   `StopCallback`, `RestartCallback`, `InterruptCallback(0/3)`
   (`libetc.rs:69-75, 417, 436, 450`) and `DMACallback(2, …)`
   (`libgpu.rs:73-75`) return libetc's context, its handlers and libgpu's
   pump. A guest call to one reaches the adapter bound at that address or
   the original over the state now in RAM. Retire the stand-ins that
   become unused.
5. [ ] **SDK3–5. One wait that spends what the original's loop would.**
   A shared helper that blocks with events delivered and spends the
   guest's budget pass by pass, as `libpad::spend_in_exception`
   (`libpad.rs:261`) accounts, restarting it at each safepoint. Use it in
   `VSync` (`libetc.rs:556-585`), libgpu's `DrawSync(0)`, full-queue and
   `MoveImage2` waits (`libgpu.rs:362-401`), and libpress's syncs
   (`libpress.rs:264-278`). Remove D66's note that these moved bof4's run
   once it no longer holds. **Run:** both games' replays.
6. [ ] **SDK11. DMA1's busy bit is CHCR1's.** `libpress.rs:33-34,
   240-244, 406`: start DMA1 and poll CHCR1 bit 24 as the original does;
   drop `m.sdk.press.out`. Revise D64's note. **Run:** bof4's FMV.

### Exit criteria

1. `cargo test --workspace` passes.
2. `psxw conform btn --chip irq`, `psxw conform bof4 --chip irq`, and the
   same for `sio0` and `mdec` where the game links them, pass.
3. `psxw build btn` and `psxw build bof4` succeed with every chip native
   as before.
4. **Human:** `psxw conform <game> --modes` and `psxw ci` for both games.

## Phase 3: the BIOS

The BIOS implemented properly. Phase 1 fixed its bugs.

1. [ ] **Record the decision** (table above, phase 3).
2. [ ] **Kernel RAM.** Lay out the A0h, B0h and C0h tables at 200h, 874h
   and 674h with stand-in entries that reach the Rust BIOS (as
   `Sdk::stand_in` routes adapter addresses, `psx-runtime/src/wasm.rs:455`);
   `GetB0Table`/`GetC0Table` return them; the vectors and kernel variables
   where psx-spx and OpenBIOS put them. A game calling through a table, or
   patching an entry, then works.
3. [ ] **BIOS8. Event control blocks in kernel RAM**, through the table
   of tables, sized by `SYSTEM.CNF` `EVENT=` (`psx-bios/src/kernel.rs`).
4. [ ] **BIOS7. Interrupt chains in RAM.** `SysEnqIntRP`/`SysDeqIntRP`
   link elements into the exception control blocks; the handler walks RAM
   only (`psx-bios/src/io.rs:684-767`); `Sdk::chain_element` keeps
   working for adapter-owned elements.
5. [ ] **BIOS4. The kernel's own interrupt handlers are chain elements**
   (`EnqueueTimerAndVblankIrqs`): root counter and vblank events, pad
   buffer filling and card pacing run when the interrupt is taken, in
   priority order, not on the device event
   (`psx-bios/src/machine.rs:385-414`). They honour `ChangeClearRCnt`,
   `ChangeClearPad` and `SetIrqAutoAck`. **Run:** both games' replays.
6. [ ] **BIOS5. The pad driver drives SIO0** for `InitPAD`/`StartPAD`
   buffers (`psx-bios/src/kernel.rs:110-126`), through the shared port
   model.
7. [ ] **BIOS6, SDK10. The card driver drives SIO0, and the cards are the
   port's.** `psx-bios/src/card.rs`: transfers paced as the BIOS paces
   them; `_card_*` and the `bu` device over it. The card images move to
   the port; libmcx's wire (`sdk-psyq/src/libmc.rs:158-173, 269`) reaches
   them through the card device's interface, not `m.kernel.cards`.
   Revise D43. **Run:** both games' save and load.
8. [ ] **BIOS9. Files over the CD controller and a device table.** `open`,
   `read`, `lseek`, `close`, `firstfile`/`nextfile` go through `AddDrv`'s
   device table (`cdrom`, `bu`, `tty`); `cdrom` reads through the CD
   controller with its timing (`psx-bios/src/bios.rs:344-436`).
9. [ ] **Every call psx-spx documents.** Group by group, each with
   `BIOS_CALL` vectors (`psxw-oracle/src/handwritten.rs`, `bios()`):
   - the C library, including `malloc`/`free`/`calloc`/`realloc` over
     `InitHeap`, number conversion, `qsort`;
   - events (`UnDeliverEvent`), timers, kernel memory;
   - files and devices (after task 8);
   - the A0h GPU functions over the GPU;
   - the A0h CD functions over the CD controller;
   - `syscall` 4 and up (the exception event), `SystemError`, exception
     hooks.
10. [ ] **What can't be done fails with the reason.** `Exec`/`LoadExec`
    of an untranslated executable, `longjmp` across translated frames
    (see [Forced](#forced)): one error each, naming why.

### Exit criteria

1. `cargo test --workspace` passes.
2. `psxw oracle capture --kind bios_call --missing` then `psxw oracle check
   btn --device bios` pass for every vector (provisional against
   OpenBIOS).
3. `psxw build btn` and `psxw build bof4` succeed.
4. **Human:** `psxw ci btn` and `psxw ci bof4`, including saves.

## Phase 4: translator correctness

1. [ ] **TR2. Registers passed outside o32 fail the lift** instead of
   arriving as 0 (`psxw-lift/src/emit.rs:128-141`). Inlining evidence
   crosses branches and joins, and counts s0–s7, fp and HI/LO
   (`analysis.rs:16-17, 109-129, 326-327`; `lib.rs:291-297, 363-382`).
   Each case that then fails gets a proper fix; open-items "Lifted SDK asm
   that reads temporaries on entry" folds in here. Update D33.
2. [ ] **TR3. A return address other than the caller's fails the lift.**
   `la ra,L; j f`, an edited saved-ra slot, a game's own setjmp/longjmp
   (`psxw-lift/src/graph.rs:345`; `emit.rs:138, 279-282, 323-324`).
3. [ ] **TR7. Variadic marshalling copies every argument** and realigns
   64-bit ones to even slots (`psxw-link/src/marshal.rs:16, 96-101`).
4. [ ] **TR8. Writable data goes to its original address.** Statics the
   address map doesn't name (`psxw-link/src/layout.rs:147-167`) are
   placed from the decomp's layout, or fail the link naming them.
5. [ ] **TR9. `memory.copy`/`memory.fill` reach the I/O hook and spend
   the budget** (`psxw-link/src/rewrite.rs:304-328`; `game.rs:487-489`;
   `bodies.rs:89-109`).
6. [ ] **TR13. Decompiled C loops get a compiler barrier,** as lifted
   loops do (`psxw-lift/src/emit.rs:167-173`), so clang can't hoist a load
   an interrupt changes.
7. [ ] **TR15. GTE lowering keeps the load delay**
   (`psxw-cc/src/gte.rs:317-324`).

### Exit criteria

1. `cargo test --workspace` passes.
2. `psxw lift btn --all`, `psxw lift bof4 --all` and `psxw difftest btn`
   pass.
3. `psxw build btn` and `psxw build bof4` succeed.
4. **Human:** `psxw ci btn` and `psxw ci bof4`.

## Phase 5: overlay residency

1. [ ] **Record the decision** (D7).
2. [ ] **TR4. Dispatch on what's loaded.** Today a stub compares two code
   words, first candidate by name wins, and single-definer, intra-overlay
   and main/overlay-overlap calls aren't checked
   (`psxw-link/src/game.rs:117, 838-868`; `psxw-build/src/game.rs:337-340,
   366-369, 423-429, 556-569`; `psxw-lift/src/program.rs:168-176`).
   Instead: the runtime keeps a residency table from the disc's writes
   into RAM (`Machine::loaded`, DMA3 and BIOS reads); every call into an
   overlay region dispatches on it; a call whose target isn't resident
   traps naming what is. Test: two overlays with identical prologues in
   one slot.

### Exit criteria

1. `cargo test --workspace` passes.
2. `psxw build btn` and `psxw build bof4` succeed.
3. **Human:** `psxw ci btn` (M2's overlay dispatch gate) and
   `psxw ci bof4`.

## Phase 6: the memory map

1. [ ] **Record the decision** (D5).
2. [ ] **TR6. The console's map.** RAM mirrored four times in 8 MB, KSEG0/1
   and KSEG2's cache control, the scratchpad's real extent, the
   expansion regions and the BIOS ROM as their own regions, bus errors
   elsewhere; function pointers into unmapped space trap
   (`psxw-link/src/rewrite.rs:199-200, 212-213, 411-416`;
   `game.rs:780-783, 795-798`; `psxw-lift/src/analysis.rs:133-143`).
   Measure the cost of the check in every access and record it.
3. [ ] **TR16. Devices and the BIOS use the same map** for guest pointers
   (`psx-devices/src/mem.rs:4, 63-72`; `psx-runtime/src/wasm.rs:196-221`).

### Exit criteria

1. `cargo test --workspace` passes.
2. `psxw build btn` and `psxw build bof4` succeed; frame time recorded
   against the previous build in Findings.
3. **Human:** `psxw ci btn` and `psxw ci bof4`.

## Phase 7: time

1. [ ] **Record the decision** (§6, D18, D66).
2. [ ] **TR5, step 1. Clang stops reshaping loops:** no unrolling,
   vectorising or loop-to-`memory.fill` rewriting
   (`psxw-cc/src/lib.rs:48`, `clang.rs:79, 95`), so pass counts track the
   source's loops.
3. [ ] **TR5, step 2. Lifted code spends the original's instruction
   counts,** per basic block, instead of a pass per loop header
   (`psxw-link/src/rewrite.rs:356-363`, `game.rs:689-708`;
   `psx-runtime/src/wasm.rs:335`, `driver.rs:144-151`).
4. [ ] **TR5, step 3. Decompiled C too.** Design how clang's blocks get
   the original's counts (the matching asm, through line tables); record
   it in Findings before building it.
5. [ ] **Re-record baselines once:** replays, golden frames, `ramdiff`
   baselines, each after showing the new timing is closer to the oracle's
   (`psxw oracle play`, `psxw oracle ramdiff`).

### Exit criteria

1. `cargo test --workspace` passes.
2. `psxw build btn` and `psxw build bof4` succeed.
3. **Human:** `psxw ci btn` and `psxw ci bof4` against the re-recorded
   baselines; `psxw oracle ramdiff` no worse than before.

## Risks

- **Phases 2, 3 and 7 move when interrupts land.** Replays can diverge.
  Investigate before re-recording (Rules).
- **TR2 breaks current builds** until each surfaced case is fixed.
- **Cost:** volatile I/O (TR1), the memory map's check in every access
  (phase 6) and per-block time (phase 7) cost speed. Measure each.
- **OpenBIOS is a provisional reference** for `BIOS_CALL` verdicts; Sony's
  quirks come from psx-spx where it documents them.

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

## Progress log

- **2026-10-30:** plan created from the shortcuts audit
  (`plans/shortcuts.md`, `3eff117`–`0bae88a`), its bugs (moved from
  open-items) and the BIOS rework.

## Findings
