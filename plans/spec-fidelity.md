# Spec fidelity

**Status:** phases 1 and 2 done but for the human's runs; phases 3 and 4 in progress; phase 6 done on its branch but for the human's runs; phase 7's steps 2 and 3 built, the adapters' time all but libgpu's, libcd's and libetc's straight-line code, baselines the human's.
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
  Done (second half): the lifter loads `psxw sdk scan`'s reach (`run::load`), and in every SDK function that reaches a chip (not just kernel RAM) every access through a base that isn't a constant or sp is volatile and fenced, in every build. btn's default build: game.wasm +71 bytes (most such functions are adapters there).
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
ticked except B15 (blocked: needs hardware); TR1's second half was built
on 2026-10-31 (`be7288c`). 1: `cargo test --workspace` passes. 2: both games
build, every chip native (`out/sf-p1-build-*.log`). 3: passes since
`7e748f0` added psx-wasm's `bios` device under test (5 vectors,
provisional against OpenBIOS). 4: **waiting on the human:**
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
3. [x] **SDK1. libpad's state into its own data.** Port records, receive
   buffers, pending results: `sdk-psyq/src/libpad.rs:48-52, 146-161`.
   Same way as task 2. **Run:** both games' replays.
   Done `2cc64f1` (agent): `pdresres.o`'s `.data`, `pddirres.o`'s records
   and buffers, found through each adapter's code; `PadInitDirect` calls
   `_padInitDirPort` as guest code; `PadStartCom` queues the real
   `_padVsyncParam`, so libpad's `ELEMENT` stand-in is gone. Chip sio0
   conformance passes in both games. Layout in Findings.
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
   supports for static functions). libcd's interrupt handler went the same
   way (`3e7f861`), and `intr_dma.o`'s handler (`0ca8dce`).
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

**Status (2026-10-31): done but for the human's runs.** 1: passes. 2:
chip irq, sio0, cd and gpu pass in both games, mdec in bof4
(`out/sf-p2-conform-*.log`). 3: both games build with every chip native.
4: **waiting on the human:** `./target/release/psxw conform btn --modes`,
`./target/release/psxw conform bof4 --modes`, `./target/release/psxw ci
btn`, `./target/release/psxw ci bof4`, and both games' replays (tasks 3, 5,
6 asked for them). What moved: libetc's and libpad's state and addresses
(D67), the waits' timing (SDK3–5: whether bof4's native and device runs
now agree is D66's open note), DMA0/DMA1 through the controller (SDK11).

## Phase 3: the BIOS

The BIOS implemented properly. Phase 1 fixed its bugs.

1. [x] **Record the decision** (table above, phase 3).
   D68, `68ea1c3`; decided without the human under the /goal.
2. [x] **Kernel RAM.** Lay out the A0h, B0h and C0h tables at 200h, 874h
   and 674h with stand-in entries that reach the Rust BIOS (as
   `Sdk::stand_in` routes adapter addresses, `psx-runtime/src/wasm.rs:455`);
   `GetB0Table`/`GetC0Table` return them; the vectors and kernel variables
   where psx-spx and OpenBIOS put them. A game calling through a table, or
   patching an entry, then works.
   Done `4ec0ec4` (agent): each entry is a stand-in in ROM space (BFC0A000h + table base + 4·function); the runtime's `bios` export reads the entry, a stand-in runs the Rust BIOS, anything else is called as guest code. Vectors, dispatchers, the garbage area, the exception vector (retail opcodes), RAM-size variables and the BOOT argument laid out; `GetB0Table`/`GetC0Table` return 874h/674h (psx-spx; OpenBIOS returns its own). The layout is in D68. A guest calling a stand-in through a function pointer reaches its thunk since `77ee6fd` (ABI 15; see Findings); the Rust BIOS's own internal calls bypass the tables. SDK adapters' BIOS calls go through them since `782a6c3`; a patched entry fails there, naming it, since adapters call the BIOS synchronously.
3. [x] **BIOS8. Event control blocks in kernel RAM**, through the table
   of tables, sized by `SYSTEM.CNF` `EVENT=` (`psx-bios/src/kernel.rs`).
   Done `5e52ad2`: after the TCBs (`psxw_abi::kernel::EVCBS`), through
   the table of tables' 120h entry, psx-spx's layout; every access is RAM.
4. [x] **BIOS7. Interrupt chains in RAM.** `SysEnqIntRP`/`SysDeqIntRP`
   link elements into the exception control blocks; the handler walks RAM
   only (`psx-bios/src/io.rs:684-767`); `Sdk::chain_element` keeps
   working for adapter-owned elements.
   Done `6155844`: `SysEnqIntRP` prepends in RAM, `SysDeqIntRP` unlinks and
   returns the element; the handler walks RAM only.
5. [x] **BIOS4. The kernel's own interrupt handlers are chain elements**
   (`EnqueueTimerAndVblankIrqs`): root counter and vblank events, pad
   buffer filling and card pacing run when the interrupt is taken, in
   priority order, not on the device event
   (`psx-bios/src/machine.rs:385-414`). They honour `ChangeClearRCnt`,
   `ChangeClearPad` and `SetIrqAutoAck`. **Run:** both games' replays.
   Done `053035d`: elements in kernel RAM run as `IrqStep::Kernel` steps:
   root counters and vblank (priority 1, acknowledge and RFE per
   `ChangeClearRCnt`), IRQ class events (3, `SetIrqAutoAck`), the pad and
   card handler (2, `StartPAD2`/`StartCARD2`, `ChangeClearPad`). Pending
   interrupts are taken at ExitCriticalSection and I_MASK writes.
6. [x] **BIOS5. The pad driver drives SIO0** for `InitPAD`/`StartPAD`
   buffers (`psx-bios/src/kernel.rs:110-126`), through the shared port
   model.
   Done `8be8b17`: the kernel's pad handler talks to each port's pad in
   SIO0's model (01h, 42h, ID, 5Ah, the halfwords; status FFh on no
   acknowledge), as OpenBIOS's `readPad`. Not modelled: the handler's own
   CPU time in the exception (libpad's adapters spend it, D60). libapi's
   priority-1 element and `EnablePAD`/`DisablePAD` (Findings) settled
   after it: see the Findings entry.
7. [x] **BIOS6, SDK10. The card driver drives SIO0, and the cards are the
   port's.** `psx-bios/src/card.rs`: transfers paced as the BIOS paces
   them; `_card_*` and the `bu` device over it. The card images move to
   the port; libmcx's wire (`sdk-psyq/src/libmc.rs:158-173, 269`) reaches
   them through the card device's interface, not `m.kernel.cards`.
   Revise D43. **Run:** both games' save and load.
   Done `5cec580`: the images are SIO0's slots (`Sio0::slots`); the driver
   moves sectors through `memcard::Responder` (FLAG, confirmed sector,
   checksum, end byte decide the outcome), one operation per vblank in the
   pad and card handler after `StartCARD2`; libmcx's wire reaches the
   port's card. D43 revised. The `bu` directory is still read from the
   image (the device table's work, task 8).
8. [x] **BIOS9. Files over the CD controller and a device table.** `open`,
   `read`, `lseek`, `close`, `firstfile`/`nextfile` go through `AddDrv`'s
   device table (`cdrom`, `bu`, `tty`); `cdrom` reads through the CD
   controller with its timing (`psx-bios/src/bios.rs:344-436`).
   Done `ee8093a` (agent): FCBs and DCBs in kernel RAM through 140h/150h (`files.rs`); every file function through the DCB's pointers, a guest driver's through `Flow::Call`; `AddDrv`/`DelDrv` and the rest of device management; `cdrom` is OpenBIOS's port of the retail driver over the CD controller with drive timing (`cdrom.rs`), its interrupts as kernel elements at priority 0; `bu` over `card.rs`; `tty` the dummy console to the host log. The boot leaves the CD driver installed, and libetc's `ResetCallback` adapter calls `_96_remove` as the original does. Reading `cdrom` files needs whole sectors (OpenBIOS's check).
9. [x] **Every call psx-spx documents.** Group by group, each with
   `BIOS_CALL` vectors (`psxw-oracle/src/handwritten.rs`, `bios()`). The
   harness's psx-wasm side is `psxw_oracle::bios::Device` (`7e748f0`): a
   vector's call on a bare psx-bios machine, v0 and the captures:
   - [x] the C library, including `malloc`/`free`/`calloc`/`realloc` over
     `InitHeap`, number conversion, `qsort` (`fe99091`, agent; Sony's
     documented bugs reproduced, 13 `disputed-bios` vectors where OpenBIOS
     differs);
   - [x] events (`UnDeliverEvent`), timers, kernel memory (`d89a101`,
     `b9db230`);
   - [x] files and devices (`ee8093a`);
   - [x] the A0h GPU functions over the GPU (`12c6040`);
   - [x] the A0h CD functions over the CD controller (`ee8093a`);
   - [x] `syscall` 4 and up (the exception event), `SystemError`, exception
     hooks (`6d62673`).

   Done (agent): 15 more vectors; `psxw oracle check btn --device bios`
   passes, 46 provisional, 0 failed, 13 disputed. OpenBIOS's CD and card
   paths need interrupts, which the runner keeps off, so unit tests cover
   them. Not implemented yet: GetSysSp, SetConf/GetConf, `bufs_cb_*`,
   `ioabort_raw`, GetSystemInfo, `_card_info_subfunc`, Krom2Offset,
   `_card_chan`, C(04h/05h/08h/09h), C(15h..19h).
10. [x] **What can't be done fails with the reason.** `Exec`/`LoadExec`
    of an untranslated executable, `longjmp` across translated frames
    (see [Forced](#forced)): one error each, naming why.

    Done `95735b7`: `Exec`, `LoadExec`, `longjmp`, `atof`, `strtod` each
    fail with their reason (`bios::unsupported`); the build lists their
    call sites.
### Exit criteria

1. `cargo test --workspace` passes.
2. `psxw oracle capture --kind bios_call --missing` then `psxw oracle check
   btn --device bios` pass for every vector (provisional against
   OpenBIOS).
3. `psxw build btn` and `psxw build bof4` succeed.
4. **Human:** `psxw ci btn` and `psxw ci bof4`, including saves.

Status (2026-10-31): 1 passes (merged at `baf9c4e`); 2 passes (46
provisional, 0 failed, 13 disputed); 3: both games build, every chip
native; conformance for `memcard`/`cd` (btn) and `cd` (bof4) passes, and
`sio0` (btn) after the merge. 4 waits for the human. What may move: fd
numbering (stdio holds 0 and 1); `printf` through the console; from boot
to `ResetCallback` the CD driver is installed (I_MASK CD|DMA, DPCR 9099h);
a syscall element at priority 0; `_patch_gte` and `_remove_ChgclrPAD`
write into ROM space (before, `_patch_gte` copied code over 0–38h).

## Phase 4: translator correctness

1. [x] **TR2. Registers passed outside o32 fail the lift** instead of
   arriving as 0 (`psxw-lift/src/emit.rs:128-141`). Inlining evidence
   crosses branches and joins, and counts s0–s7, fp and HI/LO
   (`analysis.rs:16-17, 109-129, 326-327`; `lib.rs:291-297, 363-382`).
   Each case that then fails gets a proper fix; open-items "Lifted SDK asm
   that reads temporaries on entry" folds in here. Update D33.
   Done `f71a5d1`, `1ba3f0e` (agent): evidence crosses branches and joins, both ways (a caller setting registers before, reading them after), with hi/lo and s0–s7/fp; a call that can't carry them (an unexpanded `jalr`/`bgezal`, a C caller, an address-taken callee, a read after a pointer, BIOS or kernel call) fails the lift; a register no caller sets is uninitialised and reads 0 (D27). btn expands 17 functions, bof4 25; nothing fails. The open item closes: `_spu_pitch2note` has no callers (uninitialised case), the msc02 helpers and `func_8003A2A4` are expanded into their callers. D33 reworded.
2. [x] **TR3. A return address other than the caller's fails the lift.**
   `la ra,L; j f`, an edited saved-ra slot, a game's own setjmp/longjmp
   (`psxw-lift/src/graph.rs:345`; `emit.rs:138, 279-282, 323-324`).
   Done `3400afe`, `5efaa23` (agent): ra tracked through moves, the frame, constant-address words and calls; a `jr ra` (or tail jump) with ra known not the caller's fails the lift, and where only some paths restore it the return checks at run time and traps. btn: `DrawCulledProjectedPrimitives` returns early with sp still on the scratchpad stack (a game bug; now the checked kind); libmcx's handler's `jr k0; rfe` lowers as ReturnFromException. bof4: none.
3. [x] **TR7. Variadic marshalling copies every argument** and realigns
   64-bit ones to even slots (`psxw-link/src/marshal.rs:16, 96-101`).
   Done `1209426` (agent): the census records each variadic call's argument types (`protos2.json`); C→o32 copies every word any call in the object passes, 64-bit ones at even words, and fails the link where an object's calls disagree. o32→variadic C still copies 8 words as they lie (the o32 caller's types aren't known).
4. [x] **TR8. Writable data goes to its original address.** Statics the
   address map doesn't name (`psxw-link/src/layout.rs:147-167`) are
   placed from the decomp's layout, or fail the link naming them.
   Done `8f25be0` (agent): the second — the link fails naming them. Both games put only read-only data in AUX (lift markers, switch tables).
5. [x] **TR9. `memory.copy`/`memory.fill` reach the I/O hook and spend
   the budget** (`psxw-link/src/rewrite.rs:304-328`; `game.rs:487-489`;
   `bodies.rs:89-109`).
   Done `aa3f649` (agent): a pass per word, safepoints included; in functions with an I/O hook a copy or fill touching the I/O window goes byte by byte through it; GCC's builtin `memcpy` the same path. btn game.wasm +0.4% instructions; bof4 unchanged.
6. [x] **TR13. Decompiled C loops get a compiler barrier,** as lifted
   loops do (`psxw-lift/src/emit.rs:167-173`), so clang can't hoist a load
   an interrupt changes.
   Done `4c144c8`, `328bbb0` (agent): every `for`/`while`/`do` body and every backward `goto` label starts with an empty asm memory barrier (`do { } while (0)` excepted). btn −0.8% instructions (clang stops unrolling those loops); bof4's C has no loops. Census `.i` files change: re-run `psxw census` before `ci`.
7. [x] **TR15. GTE lowering keeps the load delay**
   (`psxw-cc/src/gte.rs:317-324`).
   Done `706eaf0` (agent): the instruction after a load, `mfc2` or `cfc2` reads the old value; the template's `nop`s count. PsyQ's macros always put a `nop` there, so neither game's code changes.

### Exit criteria

1. `cargo test --workspace` passes.
2. `psxw lift btn --all`, `psxw lift bof4 --all` and `psxw difftest btn`
   pass.
3. `psxw build btn` and `psxw build bof4` succeed.
4. **Human:** `psxw ci btn` and `psxw ci bof4`.

Status (2026-10-31): 1 passes (merged at `1ba3f0e`); 2's lifts pass (3387
of 3387, 14823 of 14823); `psxw difftest btn` has 2 mismatches of 2625
(2620 pass, 3 excluded), down from 10. The seven `call_indirect` calls and
the two byte differences came with FastISel (D53, `ea8afa8`), not phase
1: the calls are direct again (`1f3d7ce`); `AnimateMessageWindowRect`'s
division stays where the source puts it (D36, `38cb9f5`); difftest takes
plain `char` from the reference flags (`23b0976`). The two left,
`DrawTextSlotImage` and `Ovl28DrawSequenceGradientRect`, are seeds on
which the original takes an address error (see Findings, "difftest's
last two mismatches"). 3 holds. 4 waits for the human, after `psxw
census` for both games (the `.i` files change again).

## Phase 5: overlay residency

1. [x] **Record the decision** (D7). Done: D69.
2. [x] **TR4. Dispatch on what's loaded.** Today a stub compares two code
   words, first candidate by name wins, and single-definer, intra-overlay
   and main/overlay-overlap calls aren't checked
   (`psxw-link/src/game.rs:117, 838-868`; `psxw-build/src/game.rs:337-340,
   366-369, 423-429, 556-569`; `psxw-lift/src/program.rs:168-176`).
   Instead: the runtime keeps a residency table from the disc's writes
   into RAM (`Machine::loaded`, DMA3 and BIOS reads); every call into an
   overlay region dispatches on it; a call whose target isn't resident
   traps naming what is. Test: two overlays with identical prologues in
   one slot.
   Done `dcc2505`, `c135310` (agent): design in D69 and §3. Residency
   units (each container's code where overlays load) hashed at build
   time; a guest flag per unit, made unknown by each disc write
   (`psxmod_loaded`, now delivered before any guest code runs again) and
   settled by hashing RAM when code needs it. Shared addresses dispatch
   through a stub; a function only one container has there checks its own
   unit on entry; `psx.not_resident` names what is resident (ABI 13). The
   lifter needed no change: the link decides from its `psxw_f_`/`psxw_d_`
   names. Test: `psxw-link/tests/overlay_residency.rs`. Cost: btn
   game.wasm +2.8% bytes, +2.8% instructions; bof4 +2.5%, +3.0% (D69).

### Exit criteria

1. `cargo test --workspace` passes.
2. `psxw build btn` and `psxw build bof4` succeed.
3. **Human:** `psxw ci btn` (M2's overlay dispatch gate) and
   `psxw ci bof4`.

Status (2026-10-31): 1 passes and 2 holds at `c135310`; 3 waits for the
human.

## Phase 6: the memory map

1. [x] **Record the decision** (D5). Done: D70.
2. [x] **TR6. The console's map.** RAM mirrored four times in 8 MB, KSEG0/1
   and KSEG2's cache control, the scratchpad's real extent, the
   expansion regions and the BIOS ROM as their own regions, bus errors
   elsewhere; function pointers into unmapped space trap
   (`psxw-link/src/rewrite.rs:199-200, 212-213, 411-416`;
   `game.rs:780-783, 795-798`; `psxw-lift/src/analysis.rs:133-143`).
   Measure the cost of the check in every access and record it.
   Done `60f2ec4`, `82fb587`, `b4baaf8`, `3adbc00` (agent); design and
   cost in D70. psxw-abi's `map` is the map. Guest code takes RAM through
   KUSEG and KSEG0 inline (one mask, one test) and calls
   `psx.map_access` for anything else; the link maps constant addresses
   in plain memory itself; function pointers go through `psx.map_code`
   outside RAM's inline view. AUX data, mods' data and synthetic code
   move out of RAM's mirrors to `8100_0000` (psxw-abi `own`). A bus error
   is trap 14. ABI 14. Cost: btn game.wasm +28.4% bytes (+5.3% gzipped),
   +35.8% instructions; bof4 +59.3% (+14.2%), +69.9%; RAM accesses cost
   nothing measurable in a V8 micro-benchmark, scratchpad accesses
   through a pointer ~2 ns more each (D70).
3. [x] **TR16. Devices and the BIOS use the same map** for guest pointers
   (`psx-devices/src/mem.rs:4, 63-72`; `psx-runtime/src/wasm.rs:196-221`).
   Done `60f2ec4`, `ee73211`, `49e442b` (agent): `Mem::read`/`write`
   go through the map (expansion reads, dropped ROM stores, a bus error
   traps), as do the native host's tools, the browser's script mods,
   types and conformance page (`psxw_guest_ptr`), the lifter's
   plain-memory test, and the differential harness (`h.map_access`; a bus
   error on both sides is agreement, counted as `both_fault`).

### Exit criteria

1. `cargo test --workspace` passes.
2. `psxw build btn` and `psxw build bof4` succeed; frame time recorded
   against the previous build in Findings.
3. **Human:** `psxw ci btn` and `psxw ci bof4`.

Status (2026-10-31): done but for the human's runs, on
`worktree-agent-ad8770dee57c0dd1f` (`60f2ec4`..`3adbc00`, rebased on
`f4eacc2`). 1 passes; 2: both games build, sizes and instruction counts in
D70; frame time needs a run, the human's (below). `psxw difftest btn`:
`DrawTextSlotImage` and `Ovl28DrawSequenceGradientRect` pass (both sides
now take the bus error); two other functions fail where the old window
hid what their seeds do (Findings). 3 and the frame times wait for the
human. Frame time, before (main) and after (this branch), each built with
its own psxw (the ABI differs):

    git checkout f4eacc2 && cargo build --release -p psxw-cli
    ./target/release/psxw census btn
    ./target/release/psxw build btn && ./target/release/psxw build bof4
    time ./target/release/psxw run btn --replay title-to-gameplay
    time ./target/release/psxw run bof4 --replay title-to-field
    git checkout <phase 6> && cargo build --release -p psxw-cli
    ./target/release/psxw build btn && ./target/release/psxw build bof4
    time ./target/release/psxw run btn --replay title-to-gameplay
    time ./target/release/psxw run bof4 --replay title-to-field

The native host doesn't pace, so wall time over the replay's frames (6,000
and 26,000) is the frame time; it includes Cranelift compiling the larger
module. Then `./target/release/psxw ci btn` and `./target/release/psxw ci
bof4`. A trap 14 (bus error) in a run is the map at work: a game access
the console would fault on, or one the map gets wrong; look at it before
anything else.

## Phase 7: time

1. [x] **Record the decision** (§6, D18, D66). Done: D71.
2. [x] **TR5, step 1. Clang stops reshaping loops:** no unrolling,
   vectorising or loop-to-`memory.fill` rewriting
   (`psxw-cc/src/lib.rs:48`, `clang.rs:79, 95`), so pass counts track the
   source's loops.
   Done `75b892e`: `CODEGEN_FLAGS` adds `-fno-unroll-loops -fno-vectorize
   -fno-slp-vectorize -mllvm -disable-loop-idiom-all` (every C → wasm
   compile: lifted, decompiled, mods, difftest). game.wasm: btn
   2,914,801 → 2,753,059 bytes (−5.5%), bof4 16,571,334 → 15,918,085
   (−3.9%).
3. [x] **TR5, step 2. Lifted code spends the original's instruction
   counts,** per basic block, instead of a pass per loop header
   (`psxw-link/src/rewrite.rs:356-363`, `game.rs:689-708`;
   `psx-runtime/src/wasm.rs:335`, `driver.rs:144-151`).
   Done `a026f30` (agent): `PSXW_SPEND(n)` at each MIPS block's start, a
   `volatile` asm on `psxw_budget`, which every link resolves to its
   budget; loop headers check it (at or below 0: a safepoint, and a
   quantum's budget added, the overrun carried). The budget is
   sixteenths of an instruction, an instruction two cycles (Findings), a
   quantum 10,000 instructions. ABI 16. Bulk memory spends nothing of its
   own (TR9's charge revised, Findings).
4. [x] **TR5, step 3. Decompiled C too.** Design how clang's blocks get
   the original's counts (the matching asm, through line tables); record
   it in Findings before building it.
   Done `a026f30` (agent), as designed (Findings, "Phase 7, step 3's
   design"): `Graph::loop_tree` (natural loops of the lifter's graph of
   the C function), `psxw_link::cost` (regions paired by nesting and
   order, a spend per straight-line segment). btn: 2,460 of 2,541 C
   functions pair. The check's distribution is in Findings and D71.
   Adapters spend their originals' instructions (`38bf58e`; the pad and
   card handlers `368670a`, `0b2c55d`), but not all yet: Findings,
   "Adapters' straight-line time".
5. [ ] **Re-record baselines once:** replays, golden frames, `ramdiff`
   baselines, each after showing the new timing is closer to the oracle's
   (`psxw oracle play`, `psxw oracle ramdiff`). The human's; commands
   below, in Status.

### Exit criteria

1. `cargo test --workspace` passes.
2. `psxw build btn` and `psxw build bof4` succeed.
3. **Human:** `psxw ci btn` and `psxw ci bof4` against the re-recorded
   baselines; `psxw oracle ramdiff` no worse than before.

Status (2026-10-31): merged to main at `9f00d2b`; 1 and 2 pass. `psxw difftest btn`: 2,620 pass, 3 excluded, the known
`AlignPadActuators` mismatch and `Ovl11ProbeActorBounds` trap, as
before. The pad and card handlers' time is done (`368670a`, `0b2c55d`;
Findings, "Adapters' straight-line time"): conformance passes for chips
irq, gpu, cd, sio0, memcard and spu in btn, and irq, gpu, cd, mdec, sio0
and spu in bof4, sio0 and memcard now observing the budget at every
look. Both games boot to their main loops and run 1,200 frames without
a trap (`psxw run <game> --check boot --check no-trap`, before the
handlers' time). libpress's time is done too (`df365f7`, chip mdec
observing the budget). Left before step 5: the straight-line code of
libgpu's, libcd's and libetc's adapters, libetc's dispatcher among
them, and first why the budget doesn't move across a vblank on chip
irq's device side (Findings). Step 5 and exit criterion 3 are the
human's.

Step 5, the human's (each game; btn's replay `title-to-gameplay`,
bof4's `title-to-field`). Run `psxw census btn` and `psxw census bof4
--no-probe` first if the `.i` files are older than this branch.

    ./target/release/psxw build btn && ./target/release/psxw build bof4
    # The oracle side (Beetle) and the comparison, before and after:
    # on main (837ebb2) and on this branch, each with its own psxw.
    ./target/release/psxw oracle play btn --replay title-to-gameplay
    ./target/release/psxw oracle ramdiff btn --replay title-to-gameplay --every 300
    ./target/release/psxw oracle play bof4 --replay title-to-field
    ./target/release/psxw oracle ramdiff bof4 --replay title-to-field --every 600
    # The replays against their checks: a reached-state check that fails
    # or a stall means the timing moved the game; re-author the presses
    # (frame-counter values) or `frames` and note it. Timed, against the
    # same on main, they give the frame time (the native host doesn't
    # pace).
    time ./target/release/psxw run btn --replay title-to-gameplay --check determinism --check no-trap --check no-shadow-io
    ./target/release/psxw run btn --replay card-load --check determinism --check no-trap
    time ./target/release/psxw run bof4 --replay title-to-field --check determinism --check no-trap --check no-shadow-io
    # The game-recorded GPU and SPU vectors (golden frames and audio):
    ./target/release/psxw oracle record btn --replay title-to-gameplay
    ./target/release/psxw oracle record bof4 --replay title-to-field
    ./target/release/psxw oracle capture --missing
    ./target/release/psxw oracle check btn --device gpu
    ./target/release/psxw oracle check btn --device spu
    ./target/release/psxw oracle check bof4 --device gpu
    ./target/release/psxw oracle check bof4 --device spu
    # Then the gates:
    ./target/release/psxw ci btn
    ./target/release/psxw ci bof4

The new timing is closer when `ramdiff` reaches the same counters with
fewer differing symbols than on main, and the frame counter's rate (the
counters at each 300th or 600th frame) is nearer Beetle's. If it is
slower or faster throughout, `guest::CYCLES_PER_INSTRUCTION` is the knob
(Findings).

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

- **2026-10-31:** phase 7, steps 2 and 3 (agent, `dee627e`..`38bf58e`):
  lifted code spends each MIPS block's instructions, decompiled C its
  loops' estimate, loop headers check (D71, ABI 16); adapters' waits and
  loops in instruction units, and some adapters' whole paths. A BIOS
  call panicked since `77ee6fd` (every conformance device side and any
  run): fixed, `dee627e`. Tests pass, both games build, difftest's
  verdicts are as before. Then libspu's and libsnd's adapters whole
  (`69ebfa8`): chip spu passes in both games. Conformance fails on time
  for chips sio0 and memcard (Findings). Human: step 5 and `ci`, after
  those handlers' time.

- **2026-10-30:** plan created from the shortcuts audit
  (`plans/shortcuts.md`, `3eff117`–`0bae88a`), its bugs (moved from
  open-items) and the BIOS rework.
- **2026-10-31:** phase 6 (agent, `60f2ec4`..`3adbc00`): the console's
  memory map (D70) for guest code, function pointers, the BIOS, the
  devices, both hosts and the differential harness; psx-wasm's own data
  at `8100_0000`; ABI 14. Tests pass, both games build; btn's difftest
  keeps its two address-error seeds as both-fault agreements and gains
  two the old window hid (Findings). Human: frame times and `ci`.
- **2026-10-31:** phase 4's difftest, 10 mismatches to 2 (agent,
  `1f3d7ce`, `23b0976`, `38cb9f5`): FastISel's `call_indirect`s are
  direct calls again, a division by a variable stays where the source
  puts it (D36), difftest compiles with the reference flags' plain
  `char`. The two left are seeds where the original takes an address
  error (Findings). Both games build; re-run `psxw census` before `ci`.
- **2026-10-31:** phase 3: BIOS7 (`6155844`), BIOS4 (`053035d`). After
  BIOS4 every chip each game places passes conformance (irq, sio0, gpu,
  cd, spu in both; memcard in btn; mdec in bof4; `out/sf-p3-conform-*`),
  and both games build with every chip native. The same holds after
  BIOS6 and task 9's groups (`out/sf-p3b-conform-*`). BIOS5 and task 10
  done.
- **2026-10-31:** phase 2: SDK2, SDK6/SDK8, SDK3–5, SDK11 done (`b92f582`..
  `8e04a5b`); SDK1 with an agent. Adapters can bind by place
  (`libgpu/sys:0x23a8`, `libetc/intr_dma:0x4c`, `libcd/bios_1:0x14a4`):
  no adapter stand-ins remain. Conformance passes: chip irq, cd and gpu in
  both games, chip mdec in bof4 (chip sio0 waits for SDK1). Phase 3: D68, BIOS8 (`5e52ad2`), the
  oracle's `bios` device (`7e748f0`; phase 1's exit criterion 3 passes);
  task 9's C library, events, timers, memory and GPU groups with an agent.
  Phase 4 with an agent.
- **2026-10-30:** phase 1 implemented (`c484570`..`7ee8c3d`): 36 items done,
  B16 struck (the shift is Beetle's), B15 blocked, TR1 half done. Device
  bugs and translator items by two agents in worktrees, cherry-picked.
  Workspace tests pass; both games build. Human runs pending (phase 1 exit
  criteria). D67 recorded for phase 2 (`c3aa557`).

## Findings

- **An instruction is two cycles** (2026-10-31, phase 7; D71). Built as
  planned, an instruction a cycle, libetc's `VSync` timed out: its
  countdown, 1 << 15 passes of 14 instructions (`vsync.o` +1A0h..+1BCh,
  +1E8h..+1FCh), is 458,766 cycles, 81% of an NTSC frame, so `VSync(0)`
  just after a vblank ran out before the next, printed "VSync: timeout"
  and acknowledged the vblank itself, natively and with chip irq on its
  device alike. On hardware it never does: the loop's three main-RAM
  loads take about five cycles each (psx-spx), so a pass is nearer 26.
  Stalls still aren't modelled one by one; `guest::CYCLES_PER_INSTRUCTION`
  = 2 is their average for the code that matters here (VSync's countdown
  becomes 1.6 frames, the old loop-pass model's 20 cycles a pass sat at
  1.2), and step 5's comparison with Beetle is where it gets checked.
  Decided under the /goal.
- **TR9's charge revised** (2026-10-31, phase 7; D71): bulk copies and
  fills spend nothing of their own. The original's instructions that move
  the bytes are already its blocks' (lifted code, where clang only ever
  merges stores the blocks count) or its region's (decompiled C), and
  clang's zero-fills of locals (D27) have none. What is lost: a GCC
  block-copy loop that the C's `memory.copy` stands for. It makes the
  function's shapes differ, so the function is one region and the loop
  counts once (`Ovl27CopySelectedPage`: 4 instructions for 4,893). 68 of
  btn's 81 unpaired C functions have fewer wasm loops than the original.
- **Step 3's check** (2026-10-31, btn @ c77a66f, difftest's 64 seeds;
  D71): over 160,119 runs where both sides returned alike, |C − T| / T
  at p50 4.9%, p75 20.0%, p90 44.5%, p95 62.3%, p99 97.2%, max 396%;
  61.9% within 10%, 79.8% within 25%; totals C/T 1.049, signed median
  0. Per function, 61.7% within 10%, 81.1% within 25%. By length (T in
  instructions): under 20, p50 0.0% p90 47.2% (58,141 runs); 20–99, 6.7%
  and 40.6%; 100–999, 6.9% and 47.4%; 1,000 and more, 15.5% and 97.2%
  (4,549). Pairing: 2,460 of btn's 2,541 C functions (573 with loops);
  81 one region. Analysis script: the report's per-pair `time` field
  (`out/difftest/<game>/<commit>/difftest.json`), units in sixteenths.
- **Adapters' straight-line time** (2026-10-31, phase 7). Conformance
  compares the device side's original, which now spends every
  instruction, with the adapter, which spent only its loops' passes, and
  it sees the difference: chip spu reads the budget after every call;
  chip irq reads root counter 2 right after `SetRCnt`; chips sio0 and
  memcard see in which vblank a quantum falls inside the pad and card
  interrupts (`sio0.interrupt-time`'s hblanks, `memcard.session`). So an
  adapter spends its original's blocks on the path it takes
  (`sdk_psyq::path`, offsets from `psxw sdk disasm`), each before what
  the block does that time can see: a root counter 2 or SIO0 access, a
  loop header (where a safepoint can fall), a guest call.
  - Done, and their chips pass: libapi's counter functions (irq);
    libgpu's timeout check (26 instructions, gpu); every libspu adapter
    and libsnd's `_SsInit` (spu, `38bf58e`, `69ebfa8`); every libpad
    adapter and its interrupt (sio0, `368670a`); libmcx's `McxStartCom`,
    elements and `_mcxDsrEntry` (memcard, `0b2c55d`).
  - Done since: libpress's whole paths (mdec, `df365f7`): every
    adapter's blocks, the waits' first look and return, a pass of 16 at
    the countdown's header, the pass that sees the MDEC or DMA1 done,
    the timeout's `printf` and resets; `DecDCTPutEnv`'s copies are 6 a
    pass (D71 had 5). Chip mdec's cases observe the budget at each
    `state`: bof4's sides agree at all 132.
  - The question, answered (2026-10-31, `a1e5cea`): the dispatcher's
    time does land. With chip irq's cases observing the budget after
    every call and every wait (btn), a vblank spends 81 instructions
    natively (`intr_vb.o`'s handler, guest code: Vcount, then the eight
    channels) and 166 on the device: those, and the dispatcher's 85
    (`startIntr`'s `setjmp` continuation, which the lift translates as
    its own function, 4; `intr.o` +26Ch's pass over line 0, 78; libapi's
    `ReturnFromException` stub, 3). They are lifted code, spending per
    block inside `guest_invoke_irq`'s call; the throw that leaves them
    doesn't undo a global. The earlier reading took a vblank's spend for
    the `VSync(-1)` after it: its 96 and 214 are `irq.sequence`'s
    `vblanks(1)` and `VSync(-1)` together (natively 81 + the callback's
    15 + 0; on the device 166 + 15 + 33), and the one wait that moves
    the budget on neither side is the one with vblank masked.
  - libetc done (`a1e5cea`): every adapter spends its original's
    blocks, the dispatcher as `IrqStep::Sdk` steps from the continuation
    to `ReturnFromException`'s stub, a quantum at its loop headers
    (+300h, +30Ch) once the budget ran out, the DMA handler (`intr_dma.o`
    +4Ch) likewise; `ResetCallback`'s clearing of the context (1,050
    passes of 4) takes safepoints as a loop does (`sdk_psyq::spin`).
    `ResetCallback` is 4,545 instructions the first time, 25 after,
    `InterruptCallback` 37 to 82 by line, `VSync(-1)` 33. The lift lowers
    `setjmp`'s call to stores, so the device spends no stub for it, and
    neither does the adapter. Two fixes on the way: `VSync`'s countdown
    made one pass too many (the original makes `t << 15` whole passes,
    then times out 8 instructions into the next), and its wait for a
    Vcount already reached is 13 instructions, not a pass. Chip irq's
    cases now observe the budget after every call and vblank wait, and
    `VSync`'s hblanks and root counter 2 exactly (they were rounded to
    frames and to 0x400 counts): both games' sides agree at all of them
    (450 budget observations in each).
  - Left: the straight-line code of libgpu's and libcd's adapters. The
    way to do each library: its chip's cases observe the budget (as
    irq's, sio0's, memcard's and mdec's now do), and its adapters spend
    their originals' blocks until the sides agree at every observation.
  - How the handlers do it (2026-10-31, `368670a`, `0b2c55d`): a
    transliteration of the original's control flow, block by block
    (`sdk_psyq::irq_path`, offsets from `psxw sdk disasm`, checked
    against the lifted C's `PSXW_SPEND`s), each block spent before what
    it does. (1) Each element's verifier spends on every walk of the
    chains (`_padIsVsync` 13, 18 or 24; `mcxresi` +0h and +40h 10, 16 or
    15). (2) A quantum passes only at a loop header
    (`sdk_psyq::irq_header`: once the budget is at or below 0, the clock
    moves a quantum and the budget gets its quantum, nothing delivered);
    `libpad::spend_in_exception` is gone, libapi's pad element takes its
    quanta at its countdown's header too. (3) Root counter 2 is read
    through the shared device at `guest_now`, as the original reads it
    (counter, mode, target, in its order), so each poll sees the cycle
    after its block; libpad's wire answers at the device's times (the
    byte's 8 × 88h cycles, /ACK 40h after it for 32 cycles, IRQ7 latched
    until acknowledged), from the guest cycle each byte goes out; so
    does libmcx's, whose /ACK event is now timed from the guest's cycle
    too. libpad's `_padFuncCurrLimit` (with the game's `bzero`: libapi's
    A0h vector, 3, or libc2's loop), `_padFuncGetTxd` and the actuator
    gates are computed, as their paths take time.
  - What else they needed: the call adapters too. With only the
    handlers exact, the budget's phase still drifted by the calls'
    instructions (`PadGetState`'s 30-odd a call, a few a frame), so where
    a quantum fell inside the interrupt still differed. Every libpad call
    and `McxStartCom` now spends its path and libapi's stubs
    (`EnterCriticalSection`'s 4, a vector's 3); `McxStartCom` enters its
    critical section, as the original does, so a quantum at its chain
    walk's header delivers nothing. Calls whose original makes no runtime
    call return quietly (`Machine::quiet`). Chips sio0 and memcard now
    observe the budget at each look and place: the two sides agree at
    every one (btn's `sio0.sequence` 95, `memcard.session` 114).
  - Not reproduced, as before: what a pad in config mode or a multitap
    would take (the info states' requests and replies, `_padFuncIntGun`,
    `_mcxFastDsrEnt`'s bulk path beyond its blocks), which these games'
    devices never answer.
- **A BIOS call panicked** (2026-10-31): `77ee6fd` kept the machine
  borrowed through `driver::run` in the runtime's `bios` import, so
  every BIOS call from guest code trapped ("RefCell already borrowed"),
  every conformance case's device side among them. Fixed, `dee627e`.
- **difftest's fuel doubled** (2026-10-31): the spends add a few
  operators per block and segment, most to the C's, and
  `FindOldestSoundVoiceByGroup`'s seed that walks 2 G voice slots ran
  out of fuel on the C side before its bus error.

- **Phase 3's choices where psx-spx is silent or OpenBIOS differs**
  (2026-10-31, agent; D68 has the layout). psx-spx followed:
  `GetB0Table`/`GetC0Table`, the exception vector and garbage area's
  retail opcodes, a device name's trailing hex digits as its port, the DCB
  entry's size at 154h, `firstfile`/`nextfile` leaving errno, `cd`,
  `undelete`, `testdevice`, `_get_errno`/`_get_error` (none in OpenBIOS),
  `putchar`'s LF as CR LF, `rename`'s borrowed FCB, `ResetEntryInt`'s
  structure, `CdGetLbn`'s -1s, `printf` through `putchar` to fd 1. Ours
  where neither decides: the CD driver reads the response before
  acknowledging; a busy driver fails a blocking call at once as
  SystemError; the dummy writes to address 0 in the interrupt handler are
  skipped; `bu`'s remove() is the dummy; the dummy tty writes to the host
  log; syscall 4+ returns v0 = 0; `SysDeqIntRP` stays OpenBIOS's working
  version so `_96_remove` works. Not modelled: BIOS CPU time; `break` →
  DeliverEvent and SystemError (the runtime's `brk`); syscall 3 doesn't
  switch threads; the boot's disc work (CdlInit, path table, executable)
  is still the runtime's through the host.
- **A stand-in called through a pointer** (phase 3 task 2; done
  2026-10-31, `77ee6fd`). Phase 6's `map_code` gave ROM space no code, and
  `untranslated` carries no arguments. Instead of widening `untranslated`,
  each stand-in has a dispatch-table slot at the top of `SYNTHETIC_CODE`
  (`BIOS_THUNKS`) whose thunk calls `psx.bios` with the stand-in's
  address; the runtime runs that function, not the table's entry, so a
  game's patch can chain to the entry it saved. 576 thunks, about 24 KB
  per game. A kernel element's function called through a pointer still
  traps (no code in ROM).

- **Phase 7, step 3's design: decompiled C's counts** (2026-10-31, under
  the /goal; recorded before building, as step 3 asks).
  - *Where the counts come from:* the original, through the lifter. It
    already builds each function's CFG from the original image, so the
    instruction count of every basic block, and its natural loops, are
    known without the matching compiler.
  - *Line tables, considered and not used:* the matching pipeline
    (cc1 → maspsx → the assembler) is each decomp's own toolchain, run in
    its own environment. psx-wasm would have to run it with `-g`, per
    game, and line info would have to survive maspsx and the assembler.
    Even with lines, GCC 2.x has no columns, and a `for` line's init, test
    and step land in different blocks in both compilers. Lines alone
    don't pair blocks.
  - *Chosen: regions by loop nest.*
    - The link sees each decompiled C function's wasm, whose `loop`s are
      the source's loops: TR13's barriers stop unrolling, and step 1
      stops vectorising and loop idioms.
    - Pair them with the original's natural loops by nesting and order.
      Where the shapes differ, the whole function is one region.
    - A region's ratio is the original's instructions in it ÷ wasm
      instructions in it.
    - Each straight-line wasm segment spends its wasm instruction count ×
      its region's ratio, in fixed point, on the budget lifted code
      spends per MIPS block (step 2).
  - *What it gets right:* a path costs the segments it runs, and a loop's
    iteration costs what the original's costs on average.
  - *What it doesn't:* how cost spreads over blocks within a region (GCC
    and clang place work differently), and load and multiply stalls (as
    in step 2).
  - *Check before re-recording baselines:* every function difftest runs
    in both tiers spends exact counts as lift and estimated counts as C.
    Compare them over the same seeds and record the distribution of the
    error.
  - *Step 2's mechanism:* the lifter writes a spend at each MIPS block's
    start as a `volatile` asm on the budget global, with no memory
    clobber, so clang keeps it in place but still optimises memory
    around it. The link checks the budget at loop headers, as today.
    Checked (2026-10-31): the pinned clang (wasi-sdk 34) accepts
    `.globaltype psxw_budget, i32` and `global.get`/`global.set` in a
    `volatile` asm, relocating them to the symbol
    (`R_WASM_GLOBAL_INDEX_LEB`). The link's global resolver
    (`psxw-link/src/game.rs`, which maps `__stack_pointer` today) needs one
    more name. An unrolled loop keeps a spend in each copy. Measure the
    cost. TR9's charge (a pass per word, including
    clang's zero-fills of locals) moves to the same units.

- **Overlay residency's limits** (2026-10-31, TR4). A unit is a
  container's whole text where overlays load: a game that overwrites part
  of an overlay and still calls the rest traps where the console would run
  it (the boot check's `overlay-mismatch` flags the case where the first
  words survive; neither game has shown it). Code the CPU copies into an
  overlay region (decompressed, moved) is noticed only the first time its
  unit is settled. Both games' overlays sit uncompressed and sector-aligned
  on their discs (bof4's inside its EMI files), the layout of a game that
  reads them straight into place, and RAM dumps under `out/` (btn frames
  4900–5000; bof4 frames 24768–25168 and an FMV, ours and Beetle's) show
  every resident overlay's whole text intact. Functions the translator
  expands into their callers run unchecked. The mods' overlay data patches
  (static and script) still decide an overlay is there from its first two
  code words: that only times a patch, but could move to the residency
  flags. Checking at each call site instead of at function entry, with a
  stub per overlay function for pointers, cost btn +6% and bof4 +10%
  (Asyncify instrumented the extra stubs). Settling every unknown candidate
  before a stub dispatches (so it never rehashes while another candidate
  is known resident) cost bof4 another 0.8% and was left out: a stub
  settles candidates in name order up to the resident one.

- **Phase 4's limits** (2026-10-31). D33: a value a function passes on
  without setting it is no evidence for its callee; calls through
  pointers carry only o32 registers; GCC's large-frame prologue saves
  s-registers through t5, so the kprintf helper looks like it reads s0–s3
  and is expanded (harmless). Dead "functions" reading registers on
  entry: split symbols (bof4 `func_80142A40`, the tail of
  `func_80142978`), data decoded as code at subsegment starts (`mfhi t6`
  at libgte msc00, libc a63, libapi c114), fragments found through
  pointers (bof4 `ovl_1936` 801B1534, `ovl_846` 801EDFA4); nothing calls
  them, function discovery could drop them. TR9 charges a pass per word,
  including clang's zero-fills of locals (D27) that the original doesn't
  have: phase 7 revisits it.
- **Clang moves divisions the source places** (TR13's difftest mismatch).
  Division by zero is undefined in C, so clang may sink a division into
  the branch that uses it; D36's R3000 division and `break` check are
  applied after clang has optimised. Decided (2026-10-31, under the
  /goal): a division by a non-constant in decompiled C stays where the
  source puts it (opaque to clang until the link lowers it as D36 says).
  Done `38cb9f5` (agent): the normalizer makes each 32-bit `/`, `%`, `/=`,
  `%=` by a non-constant a call (`__psxw_div_s` and the rest), the link
  lowers it to D36's division; `__builtin_choose_expr` keeps constant
  divisors (enum ones too), float and 64-bit divisions as C divisions.
  btn: 86 divisions, game.wasm 718160 → 718253 instructions (+0.01%),
  1999238 → 1999589 bytes; bof4's C has none. A compound assignment
  whose left side has side effects is left alone (none divide by a
  variable in either game). difftest still compares `break`s as a set;
  they could now be compared in order with the other call-outs.
- **FastISel changed the C's code since the 2026-10-29 difftest**
  (D53, `ea8afa8`, 2026-10-30; not phase 1). A difftest built with
  SelectionDAG passes everything but `AnimateMessageWindowRect`. Two
  effects: (1) a prototype-less function one call passes a pointer and
  another an int has IR call types that differ though the wasm types
  agree; FastISel calls it through the table (`call_indirect`) where
  SelectionDAG called it directly. The link now calls it directly
  (`1f3d7ce`): btn's seven `FntPrint`/`memset` calls. (2) A bitfield
  store is a read-modify-write of its whole unit (`setlen`: a word load
  and store of the tag) where SelectionDAG narrowed it to the byte store
  GCC made. Same result in RAM; a device register would see a word read
  and write where the original stores a byte. No C-tier bitfield reaches
  I/O in btn or bof4 today. `-ffine-grained-bitfield-accesses` gives
  GCC's widths but moves a lot of code (tried: other pairs then hit the
  next finding). Also: difftest compiles the C without `-g`, and under
  FastISel `-g` changes the code of ~40% of objects (D53), so the harness
  doesn't run the game's exact C.
- **difftest's last two mismatches are address errors** (2026-10-31).
  `DrawTextSlotImage` and `Ovl28DrawSequenceGradientRect` get -1 from
  `ReservePacketWorkBytes` (declared `s32`, so the harness may answer -1)
  and build a packet there. The original's `lw`/`sw` at 0xFFFFFFFF takes
  an address error; neither side models it (D5), and both treat
  misaligned accesses as byte-addressed, which agrees everywhere but at
  the window's end: a word at masked 0xFFFFFF reaches 3 bytes past the
  window instead of guest bytes 0–2, so clang's merged accesses (C) and
  the original's one-by-one ones (T) touch different bytes. With -1
  never answered, every pair passes. No compiler keeps accesses in order
  around one that faults, so C and T can't be expected to agree on these
  seeds; left as mismatches, not excluded. Options: the harness detects
  the original's address errors (an alignment check on the translation's
  `lw`/`lh`/`sw`/`sh` in harness builds) and reports the seed as such, or
  phase 6's map. In the game build the same straddle writes the first
  bytes of the runtime's data (`RUNTIME_BASE` is the window's end): for
  phase 6 (TR6).
  Settled by phase 6 (2026-10-31): 0xFFFFFFFF is KSEG2, no memory, so
  both sides take a bus error, and the harness counts a bus error on both
  sides as agreement (`both_fault`) without comparing what came before
  it. Both pass. The address error itself still isn't modelled.

- **Phase 6: where psx-spx and Beetle differ** (2026-10-31; D70 follows
  psx-spx). Beetle (`libretro.c` `PSX_MemRead*`/`PSX_MemWrite*`, `cpu.c`
  `addr_mask`) raises no bus error at all: an unmapped load reads 0, a
  store is dropped (DBE/IBE are defined, never raised). It masks KSEG0
  and KSEG1 but not KUSEG, so KUSEG above 512 MiB reaches nothing (reads
  0): neither a mirror nor psx-spx's exception. Its scratchpad test runs
  after masking, so the scratchpad answers through KSEG1 (`BF80_0000`),
  where psx-spx has none. It ignores memory control's region sizes: DEV0
  reads FFFF_FFFFh over all 8 MiB, DEV8's first 4 KiB sit in its I/O
  block and read 0, DEV1 reads 0. Of KSEG2 it has only `FFFE_0130`. Both
  agree on RAM's four mirrors, the 512 KiB ROM unmirrored and stores to
  it dropped. psx-spx doesn't say what an unconnected expansion region
  reads; Beetle's FFh/00h are taken.
- **Phase 6's limits** (2026-10-31). Memory control's size registers and
  RAM_SIZE aren't consulted (the BIOS's values are fixed); cache isolation
  (SR.IsC, the cache control bits) isn't modelled; nor are address errors
  on misaligned accesses (wasm doesn't fault). The inline path doesn't
  wrap at RAM's end: a merged access straddling `801F_FFFF` reads the
  window's next bytes (AUX), not RAM's start. Scratchpad accesses through
  a pointer (a stack moved there, as btn's
  `DrawCulledProjectedPrimitives` does) call the runtime, ~2 ns each in
  V8; a second inline test would cost ~20 bytes per access (D70). A debug
  build's DWARF for clang-only data points at its own address, which a
  debug window doesn't reach (D53). psx-wasm's own data lives at KSEG0
  `8120_0000`–`817F_FFFF`, so game code that checks a pointer against
  RAM's range (`p < 8020_0000`) would refuse a clang-only literal; none
  seen.
- **Phase 6's difftest** (2026-10-31): the harness maps as the game does,
  its scratch moved to own space (`own(0x70_0000)`, clear of RAM's
  mirrors). Two pairs fail where the old window hid what their seeds do.
  `AlignPadActuators` indexes a stack variable with a random argument
  (`(u8 *)&ports + portIndex`): C's and the original's frames differ, so
  the byte read differs; the masked window put both on zeros, the map
  puts them on RAM's mirror. `Ovl11ProbeActorBounds` loads through a
  random pointer before an early return: the original takes the bus
  error, clang sinks the loads past the return (C may; nothing in C
  faults). Both are the harness's inputs, not translation bugs; left as
  reported, not excluded.

- **libapi's pad.o installs its own vblank element** (2026-10-30, BIOS1).
  `InitPAD` and `PAD_init` call a local helper that queues a priority-1
  element: its verifier checks vblank pending and unmasked, its handler
  zeroes SIO0 CTRL (through a register pointer in libapi's `.data`) and
  spins briefly, before the BIOS's pad handler at priority 2. The adapters
  don't queue it; it matters once the BIOS's pad driver drives SIO0
  (phase 3, BIOS5). Likewise `EnablePAD`/`DisablePAD` jump into code
  `_patch_pad` finds in the Sony BIOS (B0:5Bh's address + 884h/894h,
  probably psx-spx's `pad_enable_flag`); the adapters are nops. Settle
  both in BIOS5. Settled (2026-10-31): `InitPAD`/`PAD_init` queue the
  element as the helper does (found through the `jal` at +4Ch and the
  helper's pairs), its handler and verifier adapters bound by place
  (`libapi/pad:0x1e4`, `:0x24c`); `EnablePAD`/`DisablePAD` set and clear
  the BIOS's pad-enable flag. The helper writes `next` = 0 before its
  `SysDeqIntRP`, so a second `InitPAD` cuts the elements queued after it
  off chain 1 (the kernel's timer elements here) — the original does the
  same, so it is kept.
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
  it matters only when a chip runs on its device. Decided (2026-10-31,
  under the /goal): the second — the lifter takes `psxw sdk scan`'s reach
  (`psxw_inputs::boundary::Reach`, per function), and every access but
  the stack's in a function it names is volatile, in every build. Built
  after phase 4's lifter changes merge.
- **TR12 left two callers on the old default:** psxw-modkit now takes
  plain `char` from the game's reference flags (`psxw_cc::plain_char_unsigned`,
  in the mod SDK's `flags.json`). psxw-difftest's compiler still uses
  `Clang::new`'s unsigned default; a game whose reference flags leave
  `char` signed needs it on `Clang::reference` too (phase 4's crate).
  Done `23b0976` (agent): difftest's compiler is `Clang::reference`'d.
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
  SDK doesn't name. libcd's interrupt handler (`bios_1` +14A4h), the last
  stand-in in BIOS ROM space, went the same way (`3e7f861`), and so did
  `intr_dma.o`'s handler (`0ca8dce`): no adapter stand-ins remain.
- **Phase 2's libpad layout** (SDK1, PsyQ 4.7). `pdresres.o`'s `.data`:
  `_padFunc*` pointers +08h..+30h, `_padInfoDir` +3Ch, `_padIntExec`
  +40h, `_padChanStart`/`_padChanStop` +58h/+5Ch, `_padFixResult[2]` +60h,
  `_padVsyncParam` +68h, `_padCalledIntPad` +78h; the adapters reach it
  from `PadInitDirect`'s pair at +14h, `PadStartCom`/`PadStopCom`'s at +1Ch,
  the state accessors' at +0. `pddirres.o`'s `.bss`: two `PadInfo2`
  records (F0h each), then receive buffers at +1E0h and transmit buffers
  at +228h (23h per port). `_padRestPeriod` is a common reached from
  `_padIntPad`'s pair at +ACh. Not written yet: `_padTotalCurr`, the
  actuator gates, `_padSioChan`/`_padSioState`.
- **BIOS calls vs OpenBIOS** (task 9, agent): psx-bios follows psx-spx's
  Sony behaviour where OpenBIOS differs: `todigit` past hex, `strtol`'s
  `0b`/`0o`, `atoi`'s octal, `strpbrk`/`strspn`/`strcspn`/`strtok`/`strstr`/
  `memcmp`/`memmove`'s documented bugs, `memset(…, 0)`'s 0, `GPU_cwp` and
  `gpu_abort_dma`'s results: 13 `disputed-bios` vectors kept out of the
  verdicts. Where psx-spx is silent the choices are recorded in the
  commits (heap search, `free(0)`, `realloc` failure). The runner can't pass
  a fifth argument, read registers or capture outside the scratch area,
  so callbacks, 5-argument calls, heaps, kernel memory and events are unit
  tests; extending the runner's op would make them vectors. The kernel
  heap (`alloc_kernel_memory`) is what follows the event blocks (630h
  bytes with 16 events): Sony allocates the control blocks from it, which
  task 2's layout should settle. `atof`/`strtod` (COP1, lock up) are left
  for task 10. `GPU_cw` now syncs first, which libgpu's `ResetGraph`
  reaches: a human run should confirm it.
