# Spec fidelity

**Status:** phase 1 done but for the human's runs; phases 2 and 4 in progress.
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
- [x] **B1. `SetDefaultExitFromException` (B0:18h) does nothing.**
  `psx-bios/src/bios.rs:339`. The exception handler keeps entering the old
  custom exit (`psx-runtime/src/driver.rs:51, 85`) after libetc's
  `StopCallback`. Fix: clear `kernel.custom_exit`.
  Done `c484570`.
- [x] **B2. `EnableEvent`/`DisableEvent` change closed event slots.**
  `psx-bios/src/bios.rs:303-316`: enabling a closed handle revives it;
  disabling one leaks it (`OpenEvent` reuses only unused slots,
  `kernel.rs:70`). Fix: change only slots in use.
  Done `3ead229`: as OpenBIOS's `events.c`, enabling a ready event also makes it busy again.
- [x] **B3. Memory card sector 1024 is accepted as 1023.**
  `psx-bios/src/card.rs:178, 186`. Fix: `>=`, and drop the clamp.
  Done `dc7dc05`: the spec differs from the fix above. OpenBIOS accepts sectors up to 400h (`> 0x400`); the card answers 400h with FFFFh and the driver fails it (flags 21h, event 8000h). Sector 400h now fails that way, with no clamp.
- [x] **B4. B0:15h and libapi's `PAD_init` disagree on their arguments.**
  `psx-bios/src/bios.rs:332` vs `sdk-psyq/src/libapi.rs:105`. Fix: settle
  against psx-spx (`OutdatedPadInitAndStart(type, button_dest, …)`) and
  the original `pad.o`; make both agree (BIOS1 then routes `PAD_init`
  through B0:15h).
  Done `9ed5377`: B0:15h is `PAD_init2(type, button_dest)` (psx-spx, OpenBIOS `initPadHighLevel`): types 20000000h/20000001h only, the kernel's own buffers, `PAD_dr` (B0:16h) storing at `button_dest` each vblank; libapi's `PAD_init` passes its arguments through. `InitPAD2` zero-fills; the handler stores up to 22h bytes whatever the size.
- [x] **B5. `open` on `cdrom:` hands out descriptor 16 when all are
  taken.** `psx-bios/src/bios.rs:362`. Fix: fail as the card path does.
  Done `227f8e4`: as OpenBIOS's `psxopen`, a free descriptor first (EMFILE 18h), then ENODEV for an unknown device, ENOENT for a missing CD file.
- [x] **B6. `bios::implemented()` counts calls that always error**
  (`longjmp`, `Exec`, `ReturnFromException`, `SystemError`), which the
  build's coverage check trusts. `psx-bios/src/bios.rs:104`. Fix: count
  only calls that can succeed.
  Done `d8592ec`: `implemented()` excludes `longjmp` and `Exec` (`bios::unsupported` gives the reason); the build lists such calls' sites and records the reason in `bios-calls.json` instead of failing (bof4 makes `Exec`). `SystemError` stops as the console does and `ReturnFromException` is lowered by the lift, so both stay implemented.
- [x] **B7. BIOS `printf` zero-pads negatives as `000-5`** and ignores
  `+`, space and `#`. `psx-bios/src/bios.rs:597-607`. Fix: sign before
  padding; the flags.
  Done `2a735e9`: psx-spx A(3Fh)'s conversions and prefixes (`*`, `.*`, `h`, `D`/`U`/`O`, `%n`) with C's meaning.
- [x] **B8. Stale doc on `card_queue`.** `psx-bios/src/card.rs:162`: it
  always returns true.
  Done `decdde9`: `card_queue` now refuses while the port's last operation runs (OpenBIOS: flags bit 0), so the doc holds.
- [x] **B9. The vblank counter event ignores I_MASK.**
  `psx-bios/src/machine.rs:392`; counters 0–2 are gated
  (`machine.rs:410`), and libapi's `StartRCnt(RCntCNT3)` sets bit 0
  expecting it. Fix: gate on bit 0 (phase 3 task 5 later replaces both
  with the kernel's handler).
  Done `2673b46`.

Devices (out of scope otherwise):
- [x] **B10. `GetlocP` loses a minute.** `psx-devices/src/cdc.rs:389-397`:
  the pregap is subtracted from BCD seconds with no borrow (00:58 reads
  01:00); index fixed at 1; reports the head, not the current sector.
  Fix: work in LBAs, convert once.
  Done `7716465` (agent). The index stays 1: `DiscInfo` carries only each track's index-1 start; index 0 and the pregap countdown need INDEX 00 through psxw-abi and host-native's cue parser.
- [x] **B11. GPUSTAT bits 31 and 13 follow a per-vblank field.**
  `psx-devices/src/gpu.rs:516-517`. Fix: psx-spx (bit 31 per scanline in
  240-line modes, 0 in vblank; bit 13 is 1 when not interlaced).
  Done `28f4a57` (agent). Bit 31 is 0 in vblank per psx-spx; Beetle gives `YStart & 1` there.
- [x] **B12. GPUSTAT bit 25 is wrong for DMA directions 1–3.**
  `psx-devices/src/gpu.rs:518-520`. Fix: 1 = FIFO not full, 2 = bit 28,
  3 = bit 27.
  Done `c14c58d` (agent). Beetle differs (sets the bit for directions 2 and 3 regardless).
- [x] **B13. Timer 0's dot clock runs 4–10× too fast.**
  `psx-devices/src/timers.rs:31`, `clock.rs:56-58`. Fix: divide the video
  clock by the horizontal resolution's 10/8/7/5/4.
  Done `8736711` (agent): 10/8/7/5/4 per psx-spx (Beetle ignores bit 6). Needed a hook after each GP1 write (`psx-bios/src/io.rs`, libgpu's `gp1()`) so the timers learn the display mode.
- [x] **B14. Muting or disabling the SPU silences CD audio.**
  `psx-spu/src/lib.rs:722-724`. Fix: SPUCNT bits 14–15 don't affect the CD
  input (psx-spx).
  Done `8a91a38` (agent): muting zeroes the voices (dry and reverb send) before the CD input is added.
- [ ] **B15. SPU manual-transfer writes past 32 are dropped.**
  `psx-spu/src/lib.rs:349-354`. Fix: settle with the oracle what overflow
  and late DATA writes do.
  *Blocked (2026-10-30):* the oracle can't settle it. Beetle has no FIFO (each DATA write goes to SPU RAM; its header calls the FIFO "more trouble than it's worth"), and psx-spx says only "max 32 halfwords". Needs a hardware test (M10's rig); left as is.

Renderer, SDK, translator, runtime:
- [x] ~~**B16. 15-bit colour is shifted, not expanded.**
  `psx-render-gl/src/shaders.rs:341-344` (white shows as F8F8F8). Fix:
  `(c << 3) | (c >> 2)`.~~
  *Wrong (2026-10-30):* the shift is the oracle's. Beetle zero-pads each 5-bit channel (`<< 3`, its `gpu.c` says "NOT bit-replication"), and psx-spx doesn't specify the DAC's mapping; expanding would break the golden frames.
- [x] **B17. libgpu's timeout counts every vblank** when libetc's adapters
  aren't running. `sdk-psyq/src/libgpu.rs:324-328`. Fix: read `Vcount` as
  the original does (`VSync(-1)`).
  Done `c061fcd`: libetc's `Vcount` where `VSync(-1)` reads it (found through libgpu's `jal VSync`) when libetc's adapters aren't running.
- [x] **B18. libmcx's last-byte wait is ~20 cycles, not a byte's ~1,000.**
  `sdk-psyq/src/libmc.rs:498-505`; probably a D66 regression. Fix: a
  byte's time at the port's baud rate.
  Done `0e36770`: eight bits at JOY_BAUD (~1,088 cycles at 88h).
- [x] **B19. A tail transfer out of an inlined callee exits the whole
  caller.** `psxw-lift/src/graph.rs:231-237, 260-264`; `emit.rs:198-201,
  255-258, 306-314`. Fix: return to the inlined call's return site.
  Done `0cf92f5` (agent): the target is called, then the inlined `jr ra`'s r31 dispatch. Blocks entered at computed offsets get the same dispatch, so an r31 that's neither a return site nor 0 now reaches `psxw_bad_jump` where it returned silently.
- [x] **B20. `bltzal`/`bgezal` with rs = ra test the new link value.**
  `psxw-lift/src/emit.rs:213-234`. Fix: capture the condition before
  writing r31.
  Done `76e5b25` (agent).
- [x] **B21. `psxw_lift::bios_calls()` recognises only `addiu t1`.**
  `psxw-lift/src/lib.rs:524-528`; an `ori t1` ChangeThread trampoline
  misses the Asyncify scope (`psxw-build/src/game.rs:527`). Fix: use
  `t1_after_slot` (`lib.rs:51`).
  Done `5dd692b` (agent).
- [x] **B22. GTE operands parse shifts tighter than `+`.**
  `psxw-cc/src/gte.rs:236-259`. Fix: C's precedence.
  Done `e172c80` (agent).
- [x] **B23. The EXE header's stack beats `SYSTEM.CNF`'s `STACK=`.**
  `psx-runtime/src/boot.rs:68`. Fix: check psx-spx's `LoadExec` first; if
  it writes `STACK` into the header, `STACK` wins.
  Done `22dae52`: OpenBIOS's boot writes `STACK` into the header with size 0 before `Exec`, and `Exec` leaves sp alone when the base is 0, so the header's stack is never used at boot; no `STACK` gives 801FFF00h.

### Small shortcuts

- [x] **BIOS1. libapi's pad adapters write the BIOS's pad state.**
  `sdk-psyq/src/libapi.rs:95-109`. Fix: call B0:12h, 13h and 15h.
  Done `1860277`, `cc49071`: `InitPAD`/`StartPAD` call B0:12h/13h with the original's `ChangeClearPAD(0)` and set pad.o's init flag (found through its `lui`/`sw` pair); `SetInitPadFlag`/`ReadInitPadFlag` run as original code. Not reproduced: pad.o's priority-1 vblank element (its handler zeroes SIO0 CTRL), see Findings.
- [x] **BIOS2. libcard's wrappers never call the BIOS.**
  `sdk-psyq/src/libmc.rs:60-66`. Fix: call B0:4Ah–4Ch, A0:55h/70h and
  return what they return.
  Done `7e89d5e`: `InitCARD`/`StartCARD`/`StopCARD` run as original code into the BIOS; only the functions that patch the Sony BIOS (`_patch_card`, `_patch_card2`, `_patch_card_info`, `_copy_memcard_patch`, `_ExitCard`) are adapters. B0:4Ah–4Ch follow OpenBIOS (`InitCARD2(pad_enable)`, the shared handler in or out). D43 updated.
- [x] **BIOS3. libpad reads the BIOS's chain list** to see whether its
  element is installed. `sdk-psyq/src/libpad.rs:183`. Fix: keep that in
  libpad's own state (`PadStartCom`, `PadStopCom`).
  Done `4886ad3`.
- [x] **SDK7. libetc calls handlers and callbacks with a0–a3 = 0.**
  `sdk-psyq/src/libetc.rs:215-217`. Fix: pass what the original leaves
  (I_STAT; DICR's address).
  Done `9b210b6`: a0 = the I_STAT the dispatcher read (handlers, VSync callbacks), DICR's address (DMA callbacks).
- [x] **SDK9. Messages go to the host log, not the game's `printf`.**
  `sdk-psyq/src/libgpu.rs:343, 869`; `libetc.rs:250, 578`;
  `libcd.rs:427, 638`. Fix: print through the game's `printf`, as
  libpress and libspu do.
  Done `9717931`: each message calls the `printf`/`puts` and format the original's code names, with its arguments (past the fourth on the stack, `crate::guest_call`): ResetGraph's, libgpu's timeout, libetc's unexpected-interrupt, intr-timeout and VSync-timeout messages, libcd's CD timeout and `CD_init`.
- [x] **SDK12. `DecDCTPutEnv` and transfers skip DMA bookkeeping.**
  `sdk-psyq/src/libpress.rs:42-46`. Fix: MADR/BCR after a transfer,
  DPCR's DMA1 enable, the table copy's passes.
  Done `708e754`: BCR0, MADR1/BCR1 as libpress writes them (psyq-decomp's `func_00000428`/`func_000004B8`), each transfer's end as the controller leaves it, `DecDCTPutEnv`'s 32 copy passes spent. DPCR's DMA0/DMA1 enables were already set before each transfer, as the original does.
- [x] **SDK13. `CD_datasync(0)` never waits.**
  `sdk-psyq/src/libcd.rs:659-665`. Fix: wait while DMA3 is busy, with the
  original's timeout.
  Done `2a23737`.
- [x] **SDK14. `CD_init` doesn't set the memory controller's delay
  registers.** `sdk-psyq/src/libcd.rs:46-48`. Fix: write them as the
  original does.
  Done `ba72e08`: `CD_flush` leaves COM_DELAY 1325h; `CD_getsector` 20943h/1323h then 1325h; `CD_getsector2` 21020843h/1325h.
- [x] **SDK15. `SetRCnt`, `GetRCnt`, `ResetRCnt` call the timer model.**
  `sdk-psyq/src/libapi.rs:61-63, 70, 89-90`. Fix: the counter registers
  through `set_reg`, as `StartRCnt` does.
  Done `d7bf03e`.
- [x] **SDK16. `_spu_t(3)` writes the DMA fields directly.**
  `sdk-psyq/src/libspu.rs:660-663`. Fix: MADR, BCR and CHCR4 through the
  register path.
  Done `7a80d62`: the end state through the registers. The start bit itself can't go through them: the controller would run the transfer into the device SPU, not the native one.
- [x] **REN1. A failed output texture or framebuffer is swallowed.**
  `psx-render-gl/src/lib.rs:620-621`. Fix: fail loudly, as the crate does
  elsewhere.
  Done `f664edd`: `present_frame` returns the error through `execute`.
- [x] **TR1. Lifted I/O accesses are plain memory to clang.**
  `psxw-lift/src/psxw_rt.h:23-33`, `emit.rs:416-437`; the I/O hooks come
  after clang (`psxw-link/src/rewrite.rs:195-230`). Clang may drop,
  merge or reorder register accesses. Fix: volatile for every access the
  lifter can't prove is plain RAM (constant I/O addresses, and every access
  in device-tier SDK objects).
  Partly done `ec4826f` (agent): loads and stores at constant device addresses (1F80_1000–1F80_2FFF in every mirror, expansion regions, unmapped space, FFFE_0130) are volatile and fenced on both sides; unaligned ones are fenced only. *Not done:* accesses in device-tier SDK objects through register pointers in data. Lifting runs once per game, before `psxw-build` decides which chips run on their devices, and that decision uses the lift; the register-pointer table is loaded only in `psxw-build`. Doing it needs those functions re-emitted after the chip decision, or the table passed to the lifter: a design decision (see Findings).
- [x] **TR10. o32→C wrappers don't extend narrow parameters.**
  `psxw-link/src/marshal.rs:356, 512`. Fix: sign- or zero-extend from
  the known kinds.
  Done `ad58149` (agent): `i32.extend8_s`/`extend16_s` from the prototype's kind.
- [x] **TR11. `return;` in a non-void function isn't lifted.**
  `psxw-cc/src/compile.rs:141-145`, `clang.rs:31`. Fix: match both of
  clang's return-type diagnostics and lift as for falling off the end
  (D35).
  Done `8020910` (agent).
- [x] **TR12. `-funsigned-char` is hard-wired.** `psxw-cc/src/lib.rs:81`.
  Fix: from the reference cc1 flags, as soft-float is
  (`psxw-census/src/lib.rs:58`).
  Done `744fac0` (agent): signed unless the reference flags' last `-f[no-]{un,}signed-char` says unsigned; both games' say unsigned, so nothing changes for them. psxw-modkit (its compilers, `cfront.rs`'s char check) and psxw-difftest's compiler still default to unsigned and need `Clang::reference` too (see Findings).
- [x] **TR14. Soft-float detection works from a partial list.**
  `psxw-cc/src/compile.rs:381-392`. Fix: every wasm float operation.
  Done `07c30dd` (agent).

### Exit criteria

1. `cargo test --workspace` passes.
2. `psxw build btn` and `psxw build bof4` succeed.
3. `psxw oracle check btn --device bios` passes (verdicts provisional
   against OpenBIOS).
4. **Human:** `psxw ci btn` and `psxw ci bof4` pass.

**Status (2026-10-30): done but for the human's runs.** Every item is
ticked except B15 (blocked: needs hardware) and TR1's second half (needs a
decision, Findings). 1: `cargo test --workspace` passes. 2: both games
build, every chip native (`out/sf-p1-build-*.log`). 3: can't run yet:
the oracle harness has no psx-wasm `bios` device under test ("no
implementation of bios"), so the 5 archived `BIOS_CALL` vectors wait;
building it moved to phase 3 task 9. 4: **waiting on the human:**
`./target/release/psxw ci btn` and `./target/release/psxw ci bof4`. What
moved that the runs may show: GPUSTAT bits 13/25/31 (B11, B12), timer 0's
dot clock (B13), CD audio while the SPU is muted (B14), `GetlocP` (B10),
the BIOS pad and card paths (B4, BIOS1, BIOS2: libcard's wrappers now run
the original code), the boot stack (B23), libetc's handler a0 (SDK7), the
SDK's messages (SDK9), the translator's volatile I/O and inlined tail
calls (TR1, B19).

## Phase 2: SDK state and waits

1. [x] **Record the decision** (table above, phase 2). D67, `c3aa557`;
   decided without the human under the /goal (the plan proposes it).
2. [x] **SDK2. libetc's state into its own data.** `Vcount`, `Hcount`,
   the handler, callback and DMA tables, the dispatcher's flags:
   `sdk-psyq/src/libetc.rs:4-7, 125-158, 303, 528-530`. Find them through
   the validated objects' code, as libgpu's adapters do (D61). The
   adapters read and write them there; nothing of libetc's stays in
   `m.sdk.etc` that the original keeps in RAM.
   Done `b92f582` (with SDK6's libetc half): `Lib`/`VsyncO` found through
   `intr.o`'s and `vsync.o`'s code (layout in Findings); only the
   dispatcher's I_STAT and the interrupted sp stay in the runtime.
3. [ ] **SDK1. libpad's state into its own data.** Port records, receive
   buffers, pending results: `sdk-psyq/src/libpad.rs:48-52, 146-161`.
   Same way as task 2. **Run:** both games' replays.
4. [x] **SDK6, SDK8. The real addresses.** `ResetCallback`,
   `StopCallback`, `RestartCallback`, `InterruptCallback(0/3)`
   (`libetc.rs:69-75, 417, 436, 450`) and `DMACallback(2, …)`
   (`libgpu.rs:73-75`) return libetc's context, its handlers and libgpu's
   pump. A guest call to one reaches the adapter bound at that address or
   the original over the state now in RAM. Retire the stand-ins that
   become unused.
   Done `b92f582` (libetc: the context, intr_vb's and intr_dma's own
   handlers) and `b1f07f4` (libgpu's pump: an adapter
   bound by its place in `sys.o`, `libgpu/sys:0x23a8`, which psxw-build now
   supports for static functions). libcd's interrupt handler is still a
   stand-in (`libcd::HANDLER`); the same binding would retire it (Findings).
5. [x] **SDK3–5. One wait that spends what the original's loop would.**
   A shared helper that blocks with events delivered and spends the
   guest's budget pass by pass, as `libpad::spend_in_exception`
   (`libpad.rs:261`) accounts, restarting it at each safepoint. Use it in
   `VSync` (`libetc.rs:556-585`), libgpu's `DrawSync(0)`, full-queue and
   `MoveImage2` waits (`libgpu.rs:362-401`), and libpress's syncs
   (`libpress.rs:264-278`). Remove D66's note that these moved bof4's run
   once it no longer holds. **Run:** both games' replays.
   Done `40c5bc5`: `sdk_psyq::poll` (with `budget`, `take`, `safepoint`) in
   VSync's waits, libgpu's `iterate`, libpress's `spin`; D66's note kept
   until the human's `conform bof4 --modes` shows the runs agree.
6. [x] **SDK11. DMA1's busy bit is CHCR1's.** `libpress.rs:33-34,
   240-244, 406`: start DMA1 and poll CHCR1 bit 24 as the original does;
   drop `m.sdk.press.out`. Revise D64's note. **Run:** bof4's FMV.
   Done `7bb1aeb`: DMA0 and DMA1 started through the shared controller's
   registers as libpress writes them (`Machine::dma_start_held`, completion
   held per D64); `DecDCToutSync` reads CHCR1. D64 revised.

### Exit criteria

1. `cargo test --workspace` passes.
2. `psxw conform btn --chip irq`, `psxw conform bof4 --chip irq`, and the
   same for `sio0` and `mdec` where the game links them, pass.
3. `psxw build btn` and `psxw build bof4` succeed with every chip native
   as before.
4. **Human:** `psxw conform <game> --modes` and `psxw ci` for both games.

## Phase 3: the BIOS

The BIOS implemented properly. Phase 1 fixed its bugs.

1. [x] **Record the decision** (table above, phase 3).
   D68, `68ea1c3`; decided without the human under the /goal.
2. [ ] **Kernel RAM.** Lay out the A0h, B0h and C0h tables at 200h, 874h
   and 674h with stand-in entries that reach the Rust BIOS (as
   `Sdk::stand_in` routes adapter addresses, `psx-runtime/src/wasm.rs:455`);
   `GetB0Table`/`GetC0Table` return them; the vectors and kernel variables
   where psx-spx and OpenBIOS put them. A game calling through a table, or
   patching an entry, then works.
3. [x] **BIOS8. Event control blocks in kernel RAM**, through the table
   of tables, sized by `SYSTEM.CNF` `EVENT=` (`psx-bios/src/kernel.rs`).
   Done `5e52ad2`: after the TCBs (`psxw_abi::kernel::EVCBS`), through
   the table of tables' 120h entry, psx-spx's layout; every access is RAM.
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
   `BIOS_CALL` vectors (`psxw-oracle/src/handwritten.rs`, `bios()`). First
   the harness's psx-wasm side: `psxw oracle check --device bios` has no
   `DeviceUnderTest` for `bios` (phase 1 found it), so a vector runs a call
   through `psx_bios::bios::call` on a test machine and compares RAM and
   v0 with the golden:
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
- **2026-10-30:** phase 1 implemented (`c484570`..`7ee8c3d`): 36 items done,
  B16 struck (the shift is Beetle's), B15 blocked, TR1 half done. Device
  bugs and translator items by two agents in worktrees, cherry-picked.
  Workspace tests pass; both games build. Human runs pending (phase 1 exit
  criteria). D67 recorded for phase 2 (`c3aa557`).

## Findings

- **libapi's pad.o installs its own vblank element** (2026-10-30, BIOS1).
  `InitPAD` and `PAD_init` call a local helper that queues a priority-1
  element: its verifier checks vblank pending and unmasked, its handler
  zeroes SIO0 CTRL (through a register pointer in libapi's `.data`) and
  spins briefly, before the BIOS's pad handler at priority 2. The adapters
  don't queue it; it matters once the BIOS's pad driver drives SIO0
  (phase 3, BIOS5). Likewise `EnablePAD`/`DisablePAD` jump into code
  `_patch_pad` finds in the Sony BIOS (B0:5Bh's address + 884h/894h,
  probably psx-spx's `pad_enable_flag`); the adapters are nops. Settle
  both in BIOS5.
- **OpenBIOS is the reference where psx-spx is silent, and sometimes the
  plan's fix was wrong** (B2, B3): check OpenBIOS's source (pcsx-redux's
  `src/mips/openbios`, the oracle's `8a86b7e`) before each BIOS fix.
- **TR1's second half needs a decision** (2026-10-30): which SDK objects run
  on their devices is decided in `psxw-build` after the lift, from the
  lift's results, so the lifter can't mark their register accesses
  volatile. Options: re-emit device-tier functions after the chip
  decision, or give the lifter the register-pointer table and make every
  access through it volatile in every build. Until then, SDK code that
  reaches registers through pointers in data is plain memory to clang;
  it matters only when a chip runs on its device.
- **TR12 leaves two callers on the old default:** psxw-modkit and
  psxw-difftest compile with `Clang::new` (unsigned `char`, and
  `-funsigned-char` stays in the shared UB-containment list). A game whose
  reference flags leave `char` signed needs both on `Clang::reference`.
- **Phase 2's libetc layout** (2026-10-30, from btn's PsyQ 4.7 objects;
  offsets in each object's `.text`). `intr.o` (`ResetCallback` at +0):
  the `lui`/`lw` at +0/+4 loads the driver-table pointer (`.data`+10E0h);
  the driver table (`.data`+10C0h) is {version, dma setter, interrupt,
  reset, stop, vblank setter, restart, context}; the context (psyq-decomp
  `IntrContext`) is u16 active @0, u16 dispatching @2, 11 line handlers @4,
  u16 mask @30h, u16 saved mask @32h, u32 saved DPCR @34h, `jmp_buf` @38h,
  stack @68h. The reset body's `jal startIntrVSync` is at +228h and `jal
  startIntrDMA` at +238h; the dispatcher's spurious-interrupt counter is
  the pair at +3B4h. `intr_vb.o` (`startIntrVSync` S): callback table
  pair(S+4), its handler pair(S+30h) (= S+58h: Vcount++, then the eight
  callbacks), Vcount pair(S+58h), its setter pair(S+40h) (= S+C4h, (ch, f),
  returns the old one). `intr_dma.o` (`startIntrDMA` D): table pair(D+4),
  DICR's pointer pair(D+1Ch), handler pair(D+24h) (= D+4Ch), setter
  pair(D+34h) (= D+1CCh, updates DICR's enable). With the state in RAM
  the dispatcher calls both handlers at their own addresses (SDK6), but
  only `intr_vb.o`'s runs as guest code: a native chip's guest code has no
  I/O hooks, so `intr_dma.o`'s DICR reads saw plain memory and no flags
  (chip irq's `irq.sequence` case failed: no DMA2 callback natively). It
  is an adapter bound by its place (`libetc/intr_dma:0x4c`), as libgpu's
  pump is.
- **Adapters can bind by place** (SDK8): an adapter named
  `<member>:<offset>` binds to the function at that offset in the
  member's text (`psxw-build` `adapter_name`), for static functions the
  SDK doesn't name. libcd's interrupt handler (`bios_1`'s, passed to
  `InterruptCallback(2, …)` by `CD_init`; +14A4h in btn) is the last
  stand-in in BIOS ROM space (`libcd::HANDLER`) and could go the same way.
