# Unified adapter layer, phase 1: btn and bof4

**Status:** in progress (tasks 1–4, 9a and 9b done; 5–6 harness in place)
**Depends on:** M13 (bof4 boots and plays through `title-to-field`).
**Unlocks:** [phase 2](phase2.md).
**Shared context:** [README.md](README.md): principles, vocabulary,
conformance and reporting.

## Goal

Every adapter btn and bof4 need exists, is the same code in both games,
and passes conformance. Both games then run every chip native except the
GTE. The device models stay, as the fallback and as the oracle.

## Inventory

From `psxw sdk scan` (task 2) over PsyQ 4.7's objects, joined with btn
`b37ae28` and bof4 `57f7da1`. `out/sdk/psyq-470/<game>.json` has the
per-function detail. The scan reproduces every constant-address access
the translator sees in either game's SDK code.

**141 adapters** across both games: btn needs 109, bof4 90. 78 reach a
chip or BIOS internals; 63 are state accessors. 31 exist (23 before
chip irq, task 9a).

| Library | Reach hardware | State accessors | Have |
|---|---|---|---|
| libapi | 3 | 2 | 5 |
| libcard | 2 (BIOS internals) | 0 | 2 |
| libcd | 20 | 5 | 12 |
| libetc | 8 | 0 | 8 |
| libgpu | 17 | 12 | 13 (the rest stay original over them, 9b) |
| libmcx | 1 | 18 | 4 |
| libpad | 5 | 1 | 0 |
| libpress | 3 | 0 | 0 |
| libsnd | 1 | 19 | 0 |
| libspu | 18 | 6 | 0 |

**State accessors need no adapter when the adapters keep the library's
state where the original keeps it,** in guest RAM. libsnd shows why: only
`_SsInit` touches the SPU (16 volume registers), and the 19 libsnd
accessors share `_snd_openflag` and friends with it. An `_SsInit` adapter
that writes those globals and calls the original `_SsVmInit` leaves all of
libsnd original. The scan lists accessors so each chip's adapters can
choose: state in guest RAM (accessors stay original), or state in the
runtime (accessors get adapters, as libcd's do today).

**Hardware-reaching functions, by game** (the seam each chip must cover):

| Library | btn | bof4 |
|---|---|---|
| libapi | ResetRCnt SetRCnt firstfile | same |
| libcard | InitCARD StopCARD | same |
| libcd | CdControl CdControlB CdControlF CdFlush CdInit CdRead CdReadBreak CdReadSync CdSearchFile CdSync | CD_cw CD_flush CD_init CD_initvol CD_ready CD_sync CdGetSector CdMix StCdInterrupt StUnSetRing |
| libetc | DMACallback InterruptCallback ResetCallback VSync VSyncCallback VSyncCallbacks | the same, plus SetIntrMask StopCallback |
| libgpu | ClearImage ClearOTagR DrawOTag DrawSync FntFlush FntLoad KanjiFntFlush KanjiFntOpen LoadImage MoveImage MoveImage2 PutDispEnv PutDrawEnv ResetGraph SetDispMask StoreImage | ClearImage ClearOTagR DrawOTag DrawOTagEnv DrawSync LoadImage PutDispEnv PutDrawEnv ResetGraph SetDispMask StoreImage |
| libmcx | McxStartCom | — |
| libpad | PadGetState PadInfoMode PadSetAct PadSetActAlign PadStartCom | same |
| libpress | — | DecDCTReset DecDCTin DecDCTout |
| libsnd | SsInit | SsInit |
| libspu | SpuGetAllKeysStatus SpuGetKeyStatus SpuGetNoiseVoice SpuGetReverbVoice SpuGetVoiceEnvelope SpuSetCommonAttr SpuSetKey SpuSetNoiseVoice SpuSetReverb SpuSetReverbDepth SpuSetReverbModeParam SpuSetReverbVoice SpuSetTransferStartAddr SpuSetVoiceAttr SpuWritePartly | SpuClearReverbWorkArea SpuGetNoiseVoice SpuGetReverbVoice SpuGetVoiceEnvelope SpuGetVoiceVolume SpuSetCommonAttr SpuSetKey SpuSetNoiseVoice SpuSetReverb SpuSetReverbDepth SpuSetReverbModeParam SpuSetReverbVoice SpuSetTransferStartAddr SpuSetVoiceAttr SpuSetVoiceVolume SpuWritePartly |

**libcd is the unification problem in one table.** btn calls the public
layer; bof4 calls the low layer (`CD_cw`, `CD_sync`, …) through libds,
plus streaming (`St*`). The two games share no libcd adapter today.

**The adapters btn binds today, classed by the scan:** 21 are in the set;
16 are internal (only adapted functions call them: `CdGetSector`,
`CdStatus`, …); 4 are BIOS-call trampolines (`InitCARD2`, `_bu_init`, …);
6 are riders that reach nothing themselves (`CdDataCallback` calls
libetc's `DMACallback`; `StartPAD`, `SetInitPadFlag`).

## Tasks

1. [x] **Record D59** in [docs/design.md](../../docs/design.md)'s
   decision log (README, "Decisions this plan needs"), and update §5.
2. [x] **`psxw sdk scan`: the hardware-reach analysis, in Rust.**
   - **Input:** an SDK version's relocatable library objects.
   - **Output:** for every function: the chips it reaches directly and
     through its own library, its calls (direct and through
     function-pointer tables), the data it shares with hardware-reaching
     functions, and BIOS-internal access (including through run-time
     addresses, which libcard's patching uses).
   - **Per game:** joined with the build's linked functions and with the
     calls the game's code makes, giving the needed set and the adapter
     set.
   - **Check:** it reproduces the 82 known adapters, the 22 state
     accessors included.
   - **Refine the state-accessor rule:** count data that code staying
     original reads, not all shared data.
   - Regenerate this plan's inventory from it.
3. [x] **Chip groups and the per-chip decision.**
   - `boundary/psyq.toml` names each chip's libraries.
   - `psxw-build` generalizes `adapter_or_device()` from one library to a
     chip group: a chip is native only if every placed object in its group
     that defines an adapter function is validated, and nothing outside the
     adapter set reaches the chip.
   - Expected chips go in `game.toml`.
4. [x] **Reporting.** Per chip, native or emulated and why, in
   `build.json` and in a summary line. Register accesses per chip in the
   run report. `--check no-io`.
5. [ ] **Conformance harness**, levels 1–3 (README, "Conformance"):
   - adapter cases written by symbol;
   - observation specs next to each adapter;
   - every case runs in every game that links the function;
   - `psxw conform` gains per-adapter cases beside its per-library
     scenarios;
   - a CI gate fails any adapter without a level-1 case and a level-2
     sequence for its chip.
6. [ ] **Level 4: adapter mode vs device mode.** A build switch forces
   every chip onto its device. Each game's replays run both ways and are
   compared at counter checkpoints (reuse
   `psxw oracle ramdiff --compare-frames`).
7. [ ] **Cores without the protocol** (README, "Native backends"):
   - a mixer API over `psx-spu`'s voice DSP;
   - an MDEC decode function;
   - an XA route from the drive to the mixer;
   - the GPU decoder and the drive, shared between adapters and devices.
8. [ ] **Bump the psyq-decomp pin.** libpad, libpress and libmcx were
   decompiled after `302b6ae`.
9. [ ] **Adapters, chip by chip**, in this order. Each chip is done when
   its adapters pass levels 1–3 in both games and both games report the
   chip native.

   | Order | Chip | Libraries | Adapters | Notes |
   |---|---|---|---|---|
   | a ✓ | IRQ, timers, vblank | libetc, libapi | 11 + 5, all exist | Done 2026-10-29 (progress log). Old libetc set in `111e045^`. Coexists with D45's exception path: the BIOS chains run first, a custom exit still runs after. |
   | b ✓ | GPU | libgpu | 15: 13 of the 17 that reach it, `GetODE`, `_GPU_ResetCallback` | Done 2026-10-29 (progress log). The font and kanji functions and the 12 accessors stay original over libgpu's state in guest RAM (D60). libgs rides on it. Old set (24) in `02180f4^`. |
   | c | SIO0 | libpad | 6 | Old set (7) in `609a64a^`. Must reproduce libpad's observable state machine (D40's reason for removing it). |
   | d | CD | libcd (libds and streaming ride on it) | 24 (11 exist) | **Choose the seam first:** the low layer (`CD_*`, `StCdInterrupt`) under both games, or public adapters and the low layer over one drive model. btn's 11 existing public adapters are reworked to fit. |
   | e | MDEC | libpress | 3 | Needs the decode function (task 7). |
   | f | SPU | libspu, libsnd | 26 + 1 | Needs the mixer API (task 7). The largest set. |
   | g | Memory card (PocketStation) | libmcx | 19 (4 exist) | btn only. |

10. [ ] **Both games fully native.** btn's and bof4's build reports show
    every chip native except the GTE. Every replay passes in adapter mode
    and in device mode, and level 4 compares them.

## Exit criteria (all automated)

1. **Same code everywhere:** every adapter used by btn or bof4 is one
   function in `sdk-psyq`, with no game-specific branches. A check fails
   the build on per-game adapter tables.
2. **Conformance:** every adapter in the union passes levels 1–3 in each
   game that links it.
3. **Both games native:** `psxw build` reports every chip native except
   the GTE, for both games, and `game.toml` expects it.
4. **Fallback works:** with the device switch, both games still pass their
   replays, and level 4 finds no difference beyond the documented timing
   tolerance.
5. **`psxw ci btn` and `psxw ci bof4` pass,** including the new gates.

## Risks and decision points

- **libpad's state machine (D40).** Games observe PadGetState stepping 0 →
  1 → 2 over the first frames, buffers starting `FF 00`, and port 2 polled
  a vblank after port 1. The adapter must reproduce these. Level 2 checks
  it against the original.
- **The CD seam (task 9d)** is the decision that decides whether principle
  1 holds for the CD.
- **Timing differences** between adapters (instant) and devices (modelled
  latencies) can move replay checkpoints. Compare at sync points; record a
  replay's mode if one can't hold in both.
- **The state-accessor rule** over-counts now (SpuMalloc and friends are
  likely bookkeeping) and could under-count after refining. The 82 known
  adapters are the check.
- **The oracle is provisional where the chip decides** (README, principle
  6).

## Progress log

- **2026-10-29, tasks 1–2.** D59 recorded. `psxw sdk scan` (crate
  `psxw-sdk`) reads PsyQ 4.7's 2,163 objects in 7 s: 3,140 functions, 389
  reach hardware or BIOS internals. Its check: every constant-address
  access the translator's facts see in either game's SDK code is in the
  scan's reach (0 misses). `psxw sdk disasm <fn>` prints a function with
  its relocations, for writing adapters from the objects.
- **2026-10-29, tasks 3–4.** The boundary names six chip groups (irq, gpu,
  sio0, cd, mdec, spu); every chip library is `adapter-or-device`, decided
  per chip. What touches hardware comes from the translator's facts plus
  the scan's reach table, committed as `boundary/reach/psyq-470.json`:
  libetc and libcd reach their registers through pointers in data, which
  the facts alone miss. Today: btn runs cd native, bof4 nothing (its cd
  is reached past the adapters at `CD_cw`, `CD_init`, … `StUnSetRing`).
  Because a chip never mixes, libapi's counter functions now run
  original over the timers in both games while irq is emulated; both
  still boot clean (1,200 frames, no trap, no shadow I/O). `build.json`
  has `chips` and `emulated`; `--device <chips|all>`; `[chips] native` in
  `game.toml` (btn: cd); `io_accesses` in the run report; `--check
  no-io`.
- **2026-10-29, task 9a: chip irq native in btn and bof4.** 11 libetc
  adapters (`sdk-psyq/src/libetc.rs`): `ResetCallback`, `StopCallback`,
  `RestartCallback`, `CheckCallback`, `InterruptCallback`, `DMACallback`,
  `VSyncCallback`, `VSyncCallbacks`, `GetIntrMask`, `SetIntrMask`, `VSync`,
  one code for both games, written from the 4.7 objects (`psxw sdk
  disasm`). `counter.o`'s `StartRCnt`/`StopRCnt` now set I_MASK from its
  table, as the original does. `libetc/intr` and `libetc/vsync` are
  validated; both games bind all 16 irq adapters and report irq native,
  and `game.toml` expects it (btn: cd, irq; bof4: irq).
  - **How.** libetc's state (handlers, mask, Vcount, Hcount, the VSync
    channels and DMA callbacks) is in the runtime; the interrupt
    controller, DPCR/DICR, root counter 1 and GPUSTAT stay the devices'
    registers, which the adapters use with the devices' semantics
    (`Machine::reg_read`/`reg_write`, not counted as guest I/O) because
    original libraries share them (libpad tests I_STAT, libgpu sets DPCR).
    The dispatcher runs as `IrqStep::Sdk` steps after the `SysEnqIntRP`
    chains (new `Sdk::irq_dispatch`/`irq_step`): pending lines in order,
    I_STAT acknowledged, each handler a guest call queued at the front, a
    re-check until none is left, as `intr.o`'s loop does. libetc's own
    vblank and DMA handlers are stand-in functions (`Sdk::stand_in`,
    reached through the runtime's `untranslated`), because libsnd takes
    line 0 and calls the handler it replaced. `VSync` waits the way its
    spin does: a safepoint quantum at a time, interrupts taken between
    quanta (Findings).
  - **Conformance:** 9 irq cases (`sdk-psyq/src/cases/irq.rs`), 8 level 1
    with seeded arguments and a level-2 sequence (init, vblank callbacks
    and `VSync` waits, vblank masked and unmasked, a GPU DMA callback
    around libgpu's own, root counter 2's interrupt). `psxw conform btn
    --chip irq` and `psxw conform bof4 --chip irq`: PASS, 33 case runs each
    (4 seeds). bof4 got a `[conform]` callback (`func_8013BBA8`, a leaf
    that counts its calls in a byte). `psxw conform btn --lib libetc`:
    PASS, 60 steps. The `OWED` list lost the five counter adapters.
  - **Runs:** btn and bof4, 1,200 frames, `--check boot --check no-trap
    --check no-shadow-io`: pass native and on `--device irq`. bof4
    `title-to-field` (25,300 frames, 34 s): every expect passes, at the
    same frame as on the device (counter 12390 at frame 24768). btn
    `title-to-gameplay` (6,000 frames): pass. In 1,200 frames of btn's
    boot `VSync` is called 999,299 times (a polling loop); bof4's
    replay makes 120,200 `VSync`, 97,902 `SetIntrMask` and 21,417
    `DMACallback` calls.
  - **Runtime:** the kernel's RCntCNTn events follow I_MASK now (they
    followed a flag only the old `StartRCnt` adapter set, so with
    `counter.o` original they never fired); a critical section's overdue
    events no longer step time one cycle at a time to a `Wait::Cycle`.

- **2026-10-29, task 9b: chip gpu native in btn and bof4.** 15 libgpu
  adapters (`sdk-psyq/src/libgpu.rs`), all in `sys.o`: `ResetGraph`,
  `SetDispMask`, `DrawSync`, `ClearImage`, `LoadImage`, `StoreImage`,
  `MoveImage`, `MoveImage2`, `ClearOTagR`, `DrawOTag`, `PutDrawEnv`,
  `DrawOTagEnv`, `PutDispEnv`, `GetODE`, `_GPU_ResetCallback`, one code for
  both games, written from the 4.7 object (`psxw sdk disasm`;
  psyq-decomp's C where it matches). `libgpu/sys` is validated (the same
  object, key `02b22171…`, in both games); both games report gpu native
  and `game.toml` expects it (btn: cd, gpu, irq; bof4: gpu, irq).
  - **How (D60).** libgpu's state stays in guest RAM where libgpu keeps
    it (`GEnv`, the queue, its packets), so `DrawSyncCallback`,
    `SetGraphDebug`, `ClearOTag`, `GetDrawEnv`, `SetDrawEnv`, the
    draw-area helpers and the font and kanji functions stay original over
    it. An adapter learns the guest address of the function it replaces
    (`psx.adapter(id, addr, …)`, ABI 12) and finds the object's `.data`,
    `.bss`, `_que` and its calls into libetc from the relocated operands
    of its code. Adapters make nested guest calls where libgpu does
    (`Flow::Call`): `ResetCallback`, `GetVideoMode`, `DMACallback`, the
    DrawSync callback. GP0/GP1 words go to the GPU front end; DMA2/DMA6
    run on the DMA controller with its own semantics (DPCR, DICR, IRQ3,
    the recording hook), I_MASK masked around queue work as libgpu does.
    The queue pump (DMA2's callback when work is queued) is a stand-in at
    `0xBFC7_0100`. Waits spin like libgpu's loops, a quantum per thousand
    iterations toward its timeout.
  - **Conformance:** 13 gpu cases (`sdk-psyq/src/cases/gpu.rs`), 12 level
    1 with seeded arguments (resets by mode, the display mask, DrawSync
    with and without a callback, image transfers with rectangles past
    VRAM's edges, MoveImage/MoveImage2, ordering tables, draw and display
    environments around the mode boundaries and in PAL, the field,
    `_GPU_ResetCallback`) and a level-2 sequence (a transfer before
    `ResetGraph` that waits for DPCR until libgpu's timeout, then the
    environments, the queue under a DrawSync callback, every transfer,
    the display, the lesser resets). `psxw conform btn --chip gpu` and
    `psxw conform bof4 --chip gpu`: PASS, 49 case runs each (4 seeds).
    The device observation now includes the displays presented
    (`RenderStats::present_hash`), so what GP1 set is compared too.
    `psxw conform btn --lib libgpu` (134 steps) and `--lib libetc` (60):
    PASS; `--chip irq`: PASS in both games (33 runs each).
  - **Runs:** btn and bof4, 1,200 frames, `--check boot --check no-trap
    --check no-shadow-io`: pass native and on `--device gpu`. bof4
    `title-to-field` (25,300 frames): every expect passes, at the same
    frame as on the device (counter 12390 at frame 24768). btn
    `title-to-gameplay` (6,000 frames): pass. Native against `--device
    gpu` over 1,200 frames, the renderer's command stream and the
    displays are identical (btn 395,228 words in 539 submits, bof4
    1,008,991 in 766); RAM at frame 1200 differs in 2 bytes in btn and 11
    in bof4 (Findings). btn's boot still hits libgpu's GPU timeout at
    frame 64, with the same message; its 1,200 frames make 1,609 `VSync`
    calls natively, 999,299 on the device (the timeout's spin polls
    `VSync(-1)`).

## Findings

- **The state-accessor rule, refined.** An accessor is a function that
  reaches no hardware and reads mutable library data that a
  hardware-reaching function accesses, or writes data one reads. Data
  nothing writes at run time (register-pointer tables) no longer couples.
  Accessors only need adapters when the adapters keep the state in the
  runtime; see the inventory.
- **BIOS tables.** libapi's and libcard's patches find kernel RAM through
  GetB0Table/GetC0Table (B0h:56h, 57h) and jump into patched kernel code
  (`jr 0xA000DFAC`). The scan follows both.
- **btn with its CD on the device stalls** (found by level 4, 2026-10-29).
  `psxw build btn --device cd`, then 1,500 frames: libgpu logs `GPU
  timeout: que=0, stat=d6800000, chcr=01000401` at frame 99 (a linked-list
  GPU DMA that never runs), and the frame counter never leaves 0 under
  `--device all`. btn's libcd has never run original before. Chip d owns
  it: exit criterion 4 needs it.
- **Interrupt latency is observable; `VSync` waits like its spin** (task
  9a). A first `VSync` adapter blocked event by event, as blocking runtime
  calls do (§6), so interrupts raised while a game waited were handled at
  the cycle they rose. The original spins, and the machine only takes
  interrupts at the loop's safepoints, up to a quantum (20,000 cycles)
  late. bof4 saw it: from counter 456 (the Capcom logo) libds's CD stream
  ran, an MDEC movie played (45% more sectors read, 4x the MDEC work),
  and the game fell 782 counter steps behind, so `title-to-field` missed
  its expects. With the late service, as on the device, libds re-issues
  its read every 4 frames and no movie plays. The adapter now spins a
  quantum at a time (deliver what fell due, then the handlers) and both
  modes reach every expect at the same frame. Decision: adapters that
  replace a waiting loop wait the way the loop does; the device is the
  oracle (README, principle 6). Open: hardware services interrupts within
  microseconds, so the device timing, and the replay authored over it,
  may encode an artifact of the CD controller or of libds under late
  service. Chip d and the Beetle oracle should settle it.
- **btn's boot hits libgpu's GPU timeout in every mode**, not only with
  the CD on the device: `GPU timeout: que=0, stat=56802000` at frame 99
  with irq on the device, frame 64 native. libgpu gives up after 0xF0000
  polls of `VSync(-1)`; the original's call spends a loop iteration of
  safepoint budget (its stable read of root counter 1), the adapter none,
  so polls go twice as fast in virtual time. btn then runs about 18 game
  frames ahead of device mode for the rest of a run. Replays are keyed to
  the frame counter and pass both ways; level 4 will see the offset.
- **libsnd takes line 0 and chains to libetc's handler.** `_SsStart`
  calls `InterruptCallback(0, 0)`, keeps the previous handler, installs
  its tick handler, which calls the kept one first. So libetc's own
  handlers must be callable: they are stand-in functions at
  `0xBFC7_0010` (vblank) and `0xBFC7_0020` (DMA), which the runtime runs
  when guest code calls them. Without that btn trapped at frame 99.
- **What the irq adapters don't reproduce, deliberately.** libetc's
  context pointer (`ResetCallback`, `StopCallback`, `RestartCallback`
  return `0xBFC7_0000`; neither game uses the value, and cases observe it
  as zero or not); callback arguments (zeros, where the original leaves
  I_STAT or DICR's address in a0); a ReturnFromException from a callback
  inside libetc's own handler. `GetVideoMode`/`SetVideoMode` stay guest
  code: no hardware, no state shared with the adapters.
- **PAL would time out on both sides.** `vsync.o`'s countdown is
  `(n-1) << 15` iterations, 20 cycles each under the safepoint model:
  655,380 cycles for the one-vblank wait, shorter than a PAL frame
  (677,376). A PAL game calling `VSync(0)` within 22,000 cycles after a
  vblank gets "VSync: timeout" on the device and natively alike. Both
  games here are NTSC.
- **The HLE BIOS's ResetEntryInt (B0h:18h) keeps the custom exit.** With
  libetc on the device, `StopCallback` leaves its dispatcher installed,
  which then prints "unexpected interrupt" and returns. Only bof4's `Exec`
  path calls `StopCallback`, and the BIOS doesn't support `Exec`.
- **`--check boot`'s vsync-waits** counted a build probe on `VSync`'s
  guest entry, which an adapter doesn't have; it counts `adapter.VSync`
  too now.
- **libgpu's state stays in guest RAM** (task 9b, D60). Keeping it in the
  runtime would need adapters for its 12 accessors, and two of them
  (`sys:0x1650`, `sys:0x16e8`, the draw-area packets `SetDrawArea` and
  `SetDrawEnv` build with VRAM's size) are static functions btn and bof4 name
  differently, so the build can't bind them by name. With the state where
  libgpu keeps it they stay original, and so do the font and kanji
  functions, the primitive helpers and libgs.
- **The scan's gpu set and the build's differ.** The scan counts 29
  functions: `FntFlush`, `FntLoad`, `KanjiFntOpen` and `KanjiFntFlush`
  reach the GPU only through `DrawOTag`, `LoadImage` and `DrawSync`, which
  are adapted, and the 12 accessors share state. The build's reach table
  counts only what a function's own code (or its unresolved pointer
  calls) touches, so those run as guest code over the adapters, and the
  chip is native with 13 of the 17 plus `GetODE` and
  `_GPU_ResetCallback`.
- **An unadapted libgpu function reads the I/O shadow when gpu is
  native.** A case called `GetODE`, which neither game reaches: its own
  code read GPUSTAT from the window's shadow, not the device, and
  returned 0. It is adapted now. The other `sys.o` functions that reach
  the GPU (`DrawPrim`, `SetGraphQueue`, `ClearImage2`, `LoadImage2`,
  `StoreImage2`, `DrawOTag2`) have no adapter: a game that reaches one
  falls back to the device ("reached past its adapters"), as D59 means it
  to. Phase 2 adds them.
- **libgpu's own loops take virtual time only on the device.** The
  original spends safepoint budget in its loops (ResetGraph's queue
  clear, 6,384 iterations, about 0.2 frames; the DRAWENV `memcpy` and the
  queue copies on every call); the adapters spend none. Observed over
  1,200 frames: btn's RAM differs only in libgpu's timeout deadline (one
  Vcount apart) and a dead stack byte; bof4's in dead stack and in the
  draw time its DrawSync callback measures with `VSync(1)` (9 hblanks,
  one safepoint quantum, less natively). Replays pass both ways; cases
  let a vblank pass after heavy calls. Not modelled: the budget's phase
  at a call isn't visible to the runtime, so it couldn't be exact.
- **Replays didn't catch a broken queue.** A first build ran the oldest
  queue entry for every queued call (a shadowed constant). bof4 sets a
  DrawSync callback, so all its drawing goes through the queue, and
  `title-to-field` still passed every expect: replays check game state,
  not frames. The level-2 case found it (a `StoreImage` that read
  nothing back). The native-against-device render hash (used above) and
  level 4's frame comparison are what see rendering.
- **What the gpu adapters don't reproduce, deliberately.** Debug output
  (`SetGraphDebug(n)`, n > 0: libgpu's rectangle checks and `GPU_printf`
  traces; both games set 0); waits for GPUSTAT's ready bits outside
  `DrawSync` (the front end is always ready between whole commands); the
  queue pump's address (`DMACallback(2, …)` returns the stand-in while it
  is registered); the stack contents libgpu's frames leave below `sp`.
