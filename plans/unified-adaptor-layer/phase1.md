# Unified adapter layer, phase 1: btn and bof4

**Status:** done (2026-10-29). Every task is ticked; exit criterion 5 (`psxw ci`) is the user's to run (see the progress log, "Phase 1 closes").
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
chip or BIOS internals; 63 are state accessors. 58 of them exist (23
before chip irq, task 9a; 31 before chip sio0, task 9c; chip cd, 9d,
traded libcd's 12 public-layer adapters for 8 of its drive layer here;
chip mdec, 9e, added libpress's 3; chip spu, 9f, 12 of libspu's, the
rest of libspu and libsnd original over 9 adapters below them; chip
memcard, 9g, kept 1 of libmcx's 4, its accessors original over
libmcx's state).

| Library | Reach hardware | State accessors | Have |
|---|---|---|---|
| libapi | 3 | 2 | 5 |
| libcard | 2 (BIOS internals) | 0 | 2 |
| libcd | 20 | 5 | 8 (the drive layer and the ring's interrupt; 5 more are internal, 9d) |
| libetc | 8 | 0 | 8 |
| libgpu | 17 | 12 | 13 (the rest stay original over them, 9b) |
| libmcx | 1 | 18 | 1 (and `_mcxDsrEntry`, internal; the accessors stay original over libmcx's state, 9g) |
| libpad | 5 | 1 | 6 |
| libpress | 3 | 0 | 3 (and the 3 internal ones, 9e) |
| libsnd | 1 | 19 | 0 (`SsInit` and the accessors stay original over `_SsInit`, its internal, 9f) |
| libspu | 18 | 6 | 12 (the other 6 and the accessors stay original over 8 internal ones: `spu.o`'s layer, the voice-mask helpers, `SpuSetNoiseClock`, 9f) |

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
5. [x] **Conformance harness**, levels 1–3 (README, "Conformance"):
   - adapter cases written by symbol;
   - observation specs next to each adapter;
   - every case runs in every game that links the function;
   - `psxw conform` gains per-adapter cases beside its per-library
     scenarios;
   - a CI gate fails any adapter without a level-1 case and a level-2
     sequence for its chip.
6. [x] **Level 4: adapter mode vs device mode.** A build switch forces
   every chip onto its device. Each game's replays run both ways and are
   compared at counter checkpoints (reuse
   `psxw oracle ramdiff --compare-frames`). Done: `--device all` and
   `psxw conform <game> --modes` (the replay's checks in both modes, the
   renderer commands, displays and audio over the whole run, RAM by symbol
   at the counter checkpoints); the `fallback` gate in `psxw ci`.
7. [x] **Cores without the protocol** (README, "Native backends"):
   - a mixer API over `psx-spu`'s voice DSP (done, 9f: `psx_spu::mixer`,
     each setter the register write it stands for, and `Spu::state_hash`);
   - an MDEC decode function (done, 9e: `Mdec::input`, `Mdec::output`);
   - an XA route from the drive to the mixer (done, 9d: native and device
     share one CD controller, `psx_devices::cdc`, whose decoded XA feeds the
     SPU's CD input in both modes);
   - the GPU decoder and the drive, shared between adapters and devices
     (done: 9b gives the GPU front end GP0/GP1 words directly; 9d drives
     the controller through core calls, not its registers).
8. [x] **Bump the psyq-decomp pin** (done: `654b236`). libpad, libpress and libmcx were
   decompiled after `302b6ae`.
9. [x] **Adapters, chip by chip**, in this order. Each chip is done when
   its adapters pass levels 1–3 in both games and both games report the
   chip native.

   | Order | Chip | Libraries | Adapters | Notes |
   |---|---|---|---|---|
   | a ✓ | IRQ, timers, vblank | libetc, libapi | 11 + 5, all exist | Done 2026-10-29 (progress log). Old libetc set in `111e045^`. Coexists with D45's exception path: the BIOS chains run first, a custom exit still runs after. |
   | b ✓ | GPU | libgpu | 15: 13 of the 17 that reach it, `GetODE`, `_GPU_ResetCallback` | Done 2026-10-29 (progress log). The font and kanji functions and the 12 accessors stay original over libgpu's state in guest RAM (D61). libgs rides on it. Old set (24) in `02180f4^`. |
   | c ✓ | SIO0 | libpad | 6 (+ PadStopCom) | Done 2026-10-29 (progress log). Old set (7) in `609a64a^`. Reproduces libpad's observable state machine (D40's reason for removing it) and its interrupt's time (D60). |
   | d ✓ | CD | libcd (libds and streaming ride on it) | 13: `bios_1`'s 9, `CD_getsector`, `CD_getsector2`, `StCdInterrupt`, `StUnSetRing` | Done 2026-10-29 (progress log). The seam is libcd's drive layer under both games (D62); the public layer, libds and the rest of the ring stay original over it, its state in libcd's own globals. btn's 22 public-layer adapters are gone. |
   | e ✓ | MDEC | libpress | 6: the 3 bof4 calls and the 3 internal ones | Done 2026-10-29 (progress log). bof4 only (btn links no libpress). Over the MDEC's decode function (task 7), DMA0/DMA1 completed on the shared controller, IRQ3 held to the guest's next safepoint (D64). |
   | f ✓ | SPU | libspu, libsnd | 21: `spu.o`'s 5, the 15 functions that program the SPU themselves, `_SsInit` | Done 2026-10-29 (progress log). The seam is where libspu touches the SPU, over the mixer API (task 7), its state where libspu and libsnd keep it (D63); `SpuInit`, `SpuWritePartly`, the wrappers, the accessors and all of libsnd but `_SsInit` stay original. Their time is the translated original's (budget passes, quiet returns). |
   | g ✓ | Memory card (PocketStation) | libmcx | 2: `McxStartCom`, `_mcxDsrEntry` | Done 2026-10-29 (progress log). btn only. A chip group (`memcard`) whose device is SIO0 with the BIOS's cards; the commands, `McxSync` and `McxStopCom` stay original over libmcx's state in guest RAM (D65). Old set (6) in `66ef76c`. |

10. [x] **Both games fully native.** btn's and bof4's build reports show
    every chip native except the GTE. Every replay passes in adapter mode
    and in device mode, and level 4 compares them. Done, with the
    verification state at the end of phase 1 in the progress log
    ("Phase 1 closes").

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
  a vblank after port 1. Settled by task 9c (D60): the adapters run
  libpad's engine, and its interrupt's time.
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
- **2026-10-29, task 9c: chip sio0 native in btn and bof4.** 7 libpad
  adapters (`sdk-psyq/src/libpad.rs`): `PadInitDirect`, `PadStartCom`,
  `PadStopCom`, `PadGetState`, `PadInfoMode`, `PadSetAct`,
  `PadSetActAlign`, one code for both games, written from the 4.7
  objects (`psxw sdk disasm`; psyq-decomp's matching C for `pddirres`
  and most of `pdresres`). The scan's set is the six games call;
  `PadStopCom` shares `PadStartCom`'s element, so it is adapted too. The
  eight libpad members both games link (same keys) are validated; both
  games report sio0 native, and `game.toml` expects it (btn: cd, irq,
  sio0; bof4: irq, sio0). Inventory: 37 of the 141 exist.
  - **How (D60).** libpad's direct-connection engine in Rust: the port
    records, receive buffers and pending replies in the runtime, the
    games' buffers in guest RAM. `PadStartCom` queues a `SysEnqIntRP`
    element at an address no game code has; the kernel's walk runs it as
    an `IrqStep::Sdk` step in its place (new `Sdk::chain_element`): when
    vblank is unmasked and pending, each port's pending reply, then its
    exchange, the bytes answered by a digital-pad model fed by the buttons
    latched at the vblank (`Sdk::on_vblank`, as SIO0's device latches
    them). The busy-waits' quanta (a poll seven, a refused config-mode
    command five, an empty port three) pass in the exception, and the
    guest's safepoint budget starts over after them (new
    `Regs::reset_budget`, psxw-abi `guest::BUDGET_RESET`, a guest export
    psxw-link synthesizes). The SIO0 registers aren't touched; vblank's
    I_STAT/I_MASK bits through the shared devices' registers.
  - **Conformance:** 10 sio0 cases (`sdk-psyq/src/cases/sio0.rs`), 8
    level 1 with seeded arguments (port arguments across 00h-FFh, buffer
    contents, info modes and ids, actuator tables, status bytes the game
    writes, port 2 empty, plugged, pulled) and 2 level 2 (a session:
    detection, the scripted presses, actuator calls, port 2 pulled and
    back, `PadStopCom`, buffers moved, restart; and where the game
    resumes in the frame after each vblank, in hblanks). New
    `Harness::plug_port2` (`Conform::set_replay`). `psxw conform btn
    --chip sio0` and `psxw conform bof4 --chip sio0`: PASS, 66 case runs
    each (8 seeds). `psxw conform btn --lib libpad`: PASS, 125 steps (now
    adapters against the original). Chip irq still passes in both games
    (33 runs each), and `--lib libetc` (60 steps).
  - **Runs:** btn and bof4, 1,200 frames, `--check boot --check no-trap
    --check no-shadow-io`: pass native and on `--device sio0` (btn
    999,299 `VSync` calls both ways; bof4 1,086 sectors and 763 main-loop
    passes both ways). bof4 `title-to-field`: every expect passes, at the
    same frames as on the device (counter 12390 at frame 24768); btn
    `title-to-gameplay`: pass. Both replays' frame counters, traced every
    vblank, are equal native and on the device (0 of 25,300 and 0 of
    6,000 frames differ). No SIO0 or IRQ register access in a native run.

- **2026-10-29, task 9b: chip gpu native in btn and bof4.** 15 libgpu
  adapters (`sdk-psyq/src/libgpu.rs`), all in `sys.o`: `ResetGraph`,
  `SetDispMask`, `DrawSync`, `ClearImage`, `LoadImage`, `StoreImage`,
  `MoveImage`, `MoveImage2`, `ClearOTagR`, `DrawOTag`, `PutDrawEnv`,
  `DrawOTagEnv`, `PutDispEnv`, `GetODE`, `_GPU_ResetCallback`, one code for
  both games, written from the 4.7 object (`psxw sdk disasm`;
  psyq-decomp's C where it matches). `libgpu/sys` is validated (the same
  object, key `02b22171…`, in both games); both games report gpu native
  and `game.toml` expects it (btn: cd, gpu, irq; bof4: gpu, irq).
  - **How (D61).** libgpu's state stays in guest RAM where libgpu keeps
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
- **2026-10-29, task 9d: chip cd native in btn and bof4.** 13 libcd
  adapters (`sdk-psyq/src/libcd.rs`): `bios_1`'s `CD_init`,
  `CD_initintr`, `CD_cw`, `CD_sync`, `CD_ready`, `CD_flush`, `CD_vol`,
  `CD_initvol`, `CD_datasync`; `bios_2`'s `CD_getsector`, `bios_3`'s
  `CD_getsector2`; the ring's `StCdInterrupt` (`c_011`) and `StUnSetRing`
  (`c_003`), one code for both games, written from the 4.7 objects
  (`psxw sdk disasm`). The five objects are validated (`bios_1..3` have
  one key in both games); both games report cd native and `game.toml`
  expects it (btn and bof4: cd, gpu, irq, sio0). btn's 22 public-layer
  adapters (`CdInit` … `CdSearchFile`) and their 16 validated objects are
  gone: btn's `CdRead`, `CdSearchFile` and the callbacks are original
  code now, as bof4's libds and ring are.
  - **The seam (D62).** The drive layer is where the games meet: btn
    calls libcd's public layer, bof4 libds, the ring and `CD_*` itself,
    and both go through the same `bios_1`. Everything that reaches the CD
    controller or DMA3 itself is in the set; the rest of libcd, libds and
    the ring run original over it. The other choice (public adapters and
    low-layer adapters over one drive state in the runtime) fails on
    libds, which writes the drive layer's callback slots itself
    (`DS_init`, `DS_close`).
  - **How.** State stays where libcd keeps it: `bios_1`'s `.data`
    (`CD_cbsync`, `CD_cbready`, `CD_status`, `CD_mode`, `CD_com`,
    `CD_pos`, the interrupt states, the per-command tables) and `.bss`
    (the result buffers callbacks get), and the ring's globals, all found
    from `Psyq::at` through the objects' relocated code (D61), so
    `CdStatus`, `CdMode`, `CdLastCom`, `CdLastPos`, the callback setters
    and `St*` stay original. The adapters drive the CD controller's core
    (`Cdc::issue`, `pending`, `take_response`, `acknowledge`,
    `set_enable`, `request`, `read_data`, `set_volume`), not its ports.
    Its IRQ2 reaches line 2's handler, a stand-in at `0xBFC7_0030` that
    `CD_init` installs through the guest's own `ResetCallback` and
    `InterruptCallback` (`Flow::Call`), so it runs with chip irq native or
    on its device; it takes each interrupt as `bios_1`'s routine does and
    calls the ready and sync callbacks nested. Sectors move by DMA3 on the
    shared DMA controller (DPCR, DICR's byte store, MADR/BCR/CHCR), so
    `CdDataCallback`'s and the ring's frame callback come through
    libetc's DMA handler as on the device. Waits spin a quantum at a time
    toward the original's 0x3C0000-pass timeout, poll the controller
    inside callbacks, and leave the guest's budget full (D60).
    `StCdInterrupt` runs the ring as the original does, calling
    `CdReady`, `init_ring_status` and `StFunc2`, from a frame below the
    caller's stack. The disc's DMA3 data is reported to mods' overlay
    patches in both modes now.
  - **Conformance:** 7 cd cases (`sdk-psyq/src/cases/cd.rs`), 5 level 1
    with seeded arguments (init and the handler; commands with and
    without parameters, waited for or not; reads by DMA3 whole or in two,
    `CD_getsector2`, `CD_datasync`; the volume matrix with the first
    movie's XA audio; the ring, sector by sector or once a frame, with
    its frame callback) and 2 level 2 (btn: `CdInit`, `CdSearchFile`,
    `CdRead`, the sync and ready callbacks and the accessors; bof4:
    `DsInit`, `StSetRing`, `StSetStream`, `DsRead2` of the disc's first
    movie with XA, frames taken as they come, `StUnSetRing`). The harness
    now has the game's disc and finds its first movie (the first file
    starting an STR frame: bof4 LBA 259934, btn none), function addresses
    for callbacks, and the audio in the device observation. `psxw conform
    btn --chip cd`: PASS, 19 case runs (4 seeds; the ring and stream cases
    need functions btn doesn't link); `psxw conform bof4 --chip cd`: PASS,
    22 (`cd.files` needs btn's public layer). On the merged tree `--chip
    irq` (33 runs), `--chip gpu` (49) and `--chip sio0` (34) pass in both
    games; `btn --modes` and `bof4 --modes` (level 4) pass.
  - **Runs:** btn and bof4, 1,200 frames, `--check boot --check no-trap
    --check no-shadow-io`: pass native and on `--device cd`; with libetc
    original, the CD handler under its own dispatcher, on `--device irq`
    (bof4) and `--device irq,gpu` (btn, whose `--device irq` trips chip
    gpu's shadow write, Findings). Natively no guest
    code touches the CD's registers: btn's 1,200 frames read 648 sectors
    with 86 `CD_cw`, 238 `CD_ready`, 1,294 `CD_getsector` calls; bof4's
    1,086 sectors with 971 `CD_cw`, 3,830 `CD_sync`, 977 `CD_flush`, 1,070
    `CD_getsector`, 24 `StCdInterrupt`. bof4 `title-to-field` (25,300
    frames, 19 s, 10,755 sectors): every expect passes at the same frame
    as before (counter 12390 at frame 24768). btn `title-to-gameplay`
    (6,000 frames, 8,503 sectors): pass.
  - **Infrastructure:** `psx_devices::cdc` gained its core calls (the
    register paths unchanged), `psx_devices::cd::list`, and
    `psxw_host_native::Conform` a disc and `audio()`; conformance runs
    tell the controller about the disc (`cdc_insert`). This branch first
    built the same adapter-address and `Flow::Call` mechanisms as chip gpu
    and took chip gpu's on merging.
- **2026-10-29, exit criterion 4: btn on `--device irq` passes
  `no-shadow-io`.** The 16 shadow bytes at frame 65 (Findings) weren't
  chip gpu's. They were the runtime's `psx.io_access`, which capped every
  device-library access at 4 bytes. clang merges adjacent stores, and
  lifted libspu's `_spu_init` sets each voice's volumes, pitch and start
  address (`sh` ×4) with one 8-byte store. The SPU got the volumes, and
  the pitch (3FFFh) and start address (200h) stayed in the shadow. A
  temporary probe found the store at cycle 36,660,000 (frame 64) both
  natively and on `--device irq`. Natively the shadow matched again by the
  next vblank; with libetc original, vblank 65 came first. The other reported configurations
  (`irq,cd`, `irq,sio0`) are the same. Counted by width over 1,200
  frames on `--device all`, btn makes 26 8-byte SPU stores and bof4 52;
  each lost its upper word. No other chip gets one, and no access is
  unaligned.
  - **Fix:** `Machine::io_access` (`psx-bios::io`) takes any width the
    rewrite emits (1 to 8 bytes, `kind & 0xFF`). It splits the access into
    the naturally aligned register accesses that cover it, a word at most,
    in address order, so an unaligned merge is split too. The runtime's
    `psx.io_access` calls it, and the shadow notes the whole access. The
    same code runs in both games and every mode. Tests:
    `io::tests::accesses_split_into_aligned_registers` and
    `wide_window_accesses_reach_every_register` (an 8-byte store reaches
    the voice's four registers, an unaligned one the right two, and an
    8-byte load fills the window from all four).
  - **Runs:** btn on `--device irq`, `irq,cd` and `irq,sio0`, 120 frames:
    `no-shadow-io` passes. btn and bof4, 1,200 frames, `--check boot
    --check no-trap --check no-shadow-io`: pass native and on `--device
    irq`. btn's VSync counts are unchanged from the tree before the fix
    (952,146 native and 327,620 on the device); bof4's are 3,268 both ways.
    `psxw conform btn|bof4 --chip irq|gpu`: PASS. The first `bof4 --chip
    gpu` run failed `gpu.sequence`. On the device side, the `DrawSync(0)`
    before `ResetGraph` spins libgpu's timeout (about a million
    `VSync(-1)` polls), and it hit the harness's 20 s wall-clock deadline
    for a call. Other jobs had the machine at load average 12. The rerun
    passed, 29 s for the whole chip. The deadline is wall-clock, so a
    loaded machine can fail that case.
- **2026-10-29, task 9e: chip mdec native in bof4.** 6 libpress adapters
  (`sdk-psyq/src/libpress.rs`): `DecDCTReset`, `DecDCTPutEnv`,
  `DecDCTin`, `DecDCTout`, `DecDCTinSync`, `DecDCToutSync`, every
  function of `libpress/libpress` that reaches the MDEC or DMA0/DMA1,
  written from the 4.7 object (`psxw sdk disasm`; psyq-decomp's C, for a
  libpress with the same logic at other offsets). bof4 calls the first,
  third and fourth (the scan's set); the other three are its `internal`
  ones, adapted so no function of the object reads the I/O shadow
  natively.
  `libpress/libpress` (key `c750d3ed…`, in bof4's overlay `ovl_4414`) is
  validated; bof4 reports mdec native and `game.toml` expects it (cd,
  gpu, irq, mdec, sio0). btn links no libpress. Inventory: 49 of the 141
  exist.
  - **How (D64).** libpress's tables stay in its `.data`, found from
    `Psyq::at` through the object's code with its `.rdata`, `printf` and
    `ResetCallback` (D61), so `DecDCTGetEnv` stays original, and so do
    `DecDCTinCallback`/`DecDCToutCallback` (libetc's `DMACallback`) and
    the run-length coders. The MDEC is the bus's core, driven through
    its new decode function (`Mdec::input`: commands and parameters,
    decoded at once; `Mdec::output`: decoded words, zeros past them),
    which the device's DMA0 and DMA1 now call too (phase 1 task 7's
    "MDEC decode function"). `DecDCTin`'s words go in as its DMA0 would
    deliver them (blocks of 32, at least one); DMA1 waits while a decode
    is still taking input and short of the words asked for, as on the
    device, and `DecDCTin`, a reset or a timeout lets it go. DMA0 and
    DMA1 end on the shared DMA controller (`Machine::dma_done_held`:
    DICR's flags, IRQ3), with IRQ3 held until the adapter returns
    (`Machine::hold_irq`, `release_irq` in the runtime's adapter export),
    so the slice callbacks run at the guest's next safepoint as on the
    device (Findings). DPCR gets libpress's enables; CHCR0/1 aren't
    touched. Waits spin like libpress's loops, a quantum per thousand
    iterations, to its timeout (0x100001), which prints through the
    game's `printf` and resets the MDEC and both channels. The void
    functions return what libpress leaves in v0 (DMA0's or DMA1's CHCR
    address, the reset's control word, `printf`'s result).
  - **Conformance:** 5 mdec cases (`sdk-psyq/src/cases/mdec.rs`), 4
    level 1 with seeded arguments (resets by mode, bad ones included,
    over an idle MDEC, a decode taking input or DMA1 waiting for it; random
    run-length streams in colour and monochrome at every depth, signed or
    not, sliced into DMA1 transfers of 8-384 words, before and after their
    input, past the end; random quantization tables, loaded into a busy
    MDEC to the timeout; the sync calls' polling and waiting modes, both
    timeouts) and a level-2 sequence (a movie player's calls: frames
    sliced under an out callback, DMA1 before its input, output left
    over into the next frame, new tables mid-stream, a reset with DMA1
    waiting, a short input waited out, bad options, the callbacks off).
    Observed: output buffers (pattern-filled, hashed), the command word
    `DecDCTin` rewrites, `DecDCTGetEnv`'s tables, the busy bits, callback
    counts after each call, Vcount around the timeouts (37 frames). The
    chip harness now loads the overlays a case's functions are in
    (`resident`; it refuses one over the cases' scratch, now bounded by
    `cases::SCRATCH_LEN`). `psxw conform bof4 --chip mdec`: PASS, 17 case
    runs (4 seeds). On the tree merged with main (chip cd): `--chip irq`
    (33 runs), `--chip gpu` (49), `--chip sio0` (34), `--chip cd`
    (22) pass in bof4, and `--chip irq` (33), `--chip gpu` (49), `--chip
    sio0` (34), `--chip cd` (19) in btn (4 seeds each).
  - **Runs:** bof4 `title-to-field` (25,300 frames): every expect passes
    at the same frame as on `--device mdec` (counter 12390 at frame
    24768); the movie plays from frame 1,198 (counter 762) to about
    2,000, 1 `DecDCTReset`, 91 `DecDCTin` and 1,630 `DecDCTout` calls,
    its sectors through chip cd's native path. Against `--device mdec`,
    RAM hashed every 50 frames is equal at all 506 checkpoints but the 25
    from 1,200 to 2,400, where 1 to 4 dead stack bytes differ (Findings);
    over 2,001 frames the renderer's command stream is identical (5,763,570
    words in 985 submits, same hash, same displays), so are the movie's
    slices. Natively no guest code touches the MDEC (188 register
    accesses on the device), and DMA accesses drop from 14,326 to 2,449.
    btn and bof4, 1,200 frames, `--check boot --check no-trap --check
    no-shadow-io`: pass (btn 952,146 `VSync` calls, bof4 3,268 and its
    first decoded frame: 2 `DecDCTin`, 40 `DecDCTout`); bof4 on `--device
    mdec` passes the same (10 MDEC register accesses). btn
    `title-to-gameplay` (6,000 frames): pass.

- **2026-10-29, task 9f: chip spu native in btn and bof4.** 21 adapters,
  20 in libspu (`sdk-psyq/src/libspu.rs`) and libsnd's `_SsInit`
  (`sdk-psyq/src/libsnd.rs`), one code for both games, written from the
  4.7 objects (`psxw sdk disasm`): `spu.o`'s `_spu_init`,
  `_spu_FsetRXX`, `_spu_FsetRXXa`, `_spu_Fw`, `_spu_FiDMA`, and
  `SpuSetKey`, `SpuGetKeyStatus`, `SpuGetAllKeysStatus`,
  `SpuGetVoiceEnvelope`, `SpuGetVoiceVolume`, `SpuSetVoiceVolume`,
  `SpuSetNoiseClock`, `_SpuSetAnyVoice`, `_SpuGetAnyVoice`,
  `SpuSetReverb`, `SpuSetReverbDepth`, `SpuSetReverbModeParam`,
  `SpuSetCommonAttr`, `SpuSetVoiceAttr`, `SpuClearReverbWorkArea`. Each
  game binds 19 (btn links no `Spu*VoiceVolume`, bof4 no key-status
  readers). The 17 objects they are in (16 of libspu and
  `libsnd/ssinit`, the same keys in both games) are validated; both
  games report spu native, and `game.toml` expects it, so both now run
  every chip native but the GTE (btn: cd, gpu, irq, sio0, spu; bof4: cd,
  gpu, irq, mdec, sio0, spu), with no guest register access (`--check
  no-io` passes in both). Inventory: 61 of the 141 exist.
  - **How (D63).** The seam is where libspu touches the SPU: its
    lowest layer and the functions that program `_spu_RXX` themselves;
    `SpuInit`, `_SpuInit`, `SpuStart` (which opens the DMA event and
    gives `DMACallback` the address of `_spu_FiDMA`, an adapter),
    `SpuWritePartly`, `SpuSetTransferStartAddr`, `SpuIsTransferCompleted`,
    the allocator, the `SpuSet/GetNoise/ReverbVoice` wrappers, `SsInit`
    and all of libsnd but `_SsInit` stay original. libspu's and libsnd's
    state stays in their `.data`, found through the objects' code (D61),
    and adapters call the guest where the original does: `_spu_Fw1ts`
    (libspu's delay loop), `_spu_note2pitch`, `_SpuIsInAllocateArea_`,
    `printf`, `DeliverEvent`, `WaitEvent`, the transfer callback,
    `_SsVmInit`, `SpuClearReverbWorkArea` from `SpuSetReverbModeParam`.
    The SPU is `psx-spu` through a new mixer API (`psx_spu::mixer`, task
    7: each setter the register write it stands for) brought up to now
    first (`Machine::with_spu`); DMA4 programs MADR4/BCR4 on the shared
    controller, moves its words through the core's transfer port and
    holds its completion (D64). The original's time: the adapters spend
    the guest's safepoint budget as the translated loops do (new guest
    exports `guest_budget_get`/`set`, `Regs::budget`/`set_budget`), and
    return without a safepoint (`Machine::quiet`) where the original
    makes no runtime call (Findings).
  - **Conformance:** 14 spu cases (`sdk-psyq/src/cases/spu.rs`), 12
    level 1 with seeded arguments (the reset cold and hot; writes by DMA
    and through the FIFO at any address and length, with and without the
    transfer callback; registers by index and as SPU RAM addresses;
    every reverb work area, bad modes included, with the FIFO mode and a
    callback set aside; keys on, off and invalid between vblanks, in
    register-queue mode too; key status of any mask; random voice and
    common attributes across their ranges and past them; voice volumes
    as levels and sweeps; the noise and reverb masks by every op and the
    noise clock; reverb modes with and without clearing, delay time,
    feedback, depth; `SsInit` and `_SsInit`), each with the safepoint
    budget's phase moved first by calls to `_spu_Fw1ts`, and 2 level 2
    (a sound driver over libsnd and libspu: init through `SsInit`,
    samples up by DMA and FIFO, a hall reverb, six voices by note through
    the reverb and the noise, heard and released; transfers back to back
    with and without the callback, polled across vblanks, around a
    reverb clear, a voice playing over them). Observed after every step:
    return values, the budget left, the SPU's whole state at the core
    (new `Spu::state_hash`, runtime export `psxw_spu_hash`, now in
    `Harness::device` for every chip) and the audio, RAM below the
    cases' scratch in 64 KB blocks, callback counts. `psxw conform btn
    --chip spu` and `psxw conform bof4 --chip spu`: PASS, 91 case runs
    each (8 seeds; one case skipped in each, for the readers the game
    doesn't link), with identical observations across the two games.
    `--lib libspu` (11 steps) and `--lib libsnd` (11): PASS. On the tree
    merged with main (chip mdec): `--chip irq` (33 runs), `gpu` (49),
    `sio0` (34), `cd` (19 in btn, 22 in bof4) and `mdec` (17, bof4) pass
    in both games (4 seeds).
  - **Runs:** btn and bof4, 1,200 frames, `--check boot --check no-trap
    --check no-shadow-io --check no-io`: pass, and `--check determinism`
    (RAM and audio). Against `--device spu` (which passes the same boot
    checks, 44,969 and 52,557 SPU register accesses) the audio is the
    same (btn 884,693 frames, 511,326 audible, hash `37d8f3c0…`; bof4
    884,693 frames, hash `ace3f991…`), `VSync` calls are equal (btn
    952,146, bof4 3,268), and RAM at frame 1200 differs only in dead
    stack (5 bytes in btn, 13 in bof4). bof4 `title-to-field` (25,300
    frames): every expect passes at the device's frames (counter 12390 at
    frame 24768), the frame counter is equal to `--device spu`'s every
    frame, and so is the audio (18,652,285 frames, 17,299,810 audible,
    the same hash). btn `title-to-gameplay` (6,000 frames): pass, frame
    counter equal every frame, `VSync` calls equal (131,784,317).
    Level 4 (`psxw conform <game> --modes`, every chip native against
    every chip on its device): PASS in both; bof4's checkpoints all fall
    at the same frames, the bytes that differ (235-247 in 18 symbols)
    are libetc's, libpad's and libcd's runtime-side state, none of
    libspu's or libsnd's.

- **2026-10-29, task 9g: chip memcard native in btn.** libmcx, the
  PocketStation library, is a chip group, `memcard` (D65), with 2
  adapters (`sdk-psyq/src/libmc.rs`): `McxStartCom` and `_mcxDsrEntry`,
  written from the 4.7 objects (`psxw sdk disasm`; psyq-decomp's C for
  `mcxinit`, `mcxmain` and most of `mcxresi`). `libmcx/mcxinit`,
  `mcxmain`, `mcxresi` and `mcxdsr` are validated (the old entries for
  `mcxcmd2`, `mcxcmd23`, `mcxcmd24` went with their adapters); btn
  reports memcard native and `game.toml` expects it (cd, gpu, irq,
  memcard, sio0). bof4 links no libmcx. The six old libmcx adapters
  (`McxStartCom` doing nothing, `McxStopCom`, `McxSync`, `McxCardType`,
  `McxAllInfo`, `McxExecApl` answering at once) are gone. Inventory: 46
  of the 141 exist.
  - **How (D65).** libmcx's state stays in its `.data` and common blocks,
    found from `McxStartCom`'s and `_mcxDsrEntry`'s places (D61) through
    `mcxresi`'s elements, its vblank handler's and `_mcxStepSio`'s
    operands, so the 16 command functions btn calls, their Tx/Rx
    callbacks, `McxSync` (its `McxSync(0)` spin included) and
    `McxStopCom` stay original. `McxStartCom` appends libmcx's vblank
    element to priority 2's chain through the table of tables, as
    `mcxinit` does; the kernel runs libmcx's two elements as adapter
    steps (`chain_element` maps its own records): the vblank handler
    (time-outs, `SetUIFS`'s retries, starting a reserved transfer) and
    the /ACK element, whose handler is `_mcxDsrEntry`, called as a guest
    function. `_mcxDsrEntry` is `_mcxStepSio` over a wire (the slot
    selected, the byte received, the port's interrupt flag), the
    command's callbacks called where it calls them (`Flow::Call`), the
    bulk path (`_mcxFastDsrEnt` from C) and the ends. A slot's card is
    the BIOS's image answering as a standard card
    (`psx_devices::memcard::Responder`, new: 81h and the FLAG, `R`/`W`,
    every other command refused). The address byte's wait spends a
    quantum and starts the guest's budget over (D60); an /ACK raises IRQ7
    at its time, left for the next interrupt to take, as SIO0's device
    does.
  - **The device side, made to run.** The HLE kernel keeps its exception
    control blocks in RAM (psxw-abi `kernel::EXCB`, from 100h), walks
    what is linked there after its own chain and unlinks it in
    `SysDeqIntRP`: original `McxStartCom` links its element itself. SIO0's
    device gets the two card slots (Beetle's `InputDevice_Memcard`, /ACK
    100h cycles after a byte), over the BIOS's images, which `psx-bios`
    passes to every SIO0 access and loads when a card's address first
    goes out (Findings). The root
    counters report mode bits 11 and 12 (target and FFFFh reached since
    the last read, cleared by it), which libmcx's wait on root counter 2
    needs (Findings). DEVICE_LIBRARIES has libmcx.
  - **Conformance:** 3 memcard cases (`sdk-psyq/src/cases/memcard.rs`), 2
    level 1 with seeded arguments (`McxStartCom` once or twice, vblank
    masked first or not, then a card-type query to either slot; one of
    the 16 commands with seeded arguments to a seeded port, slot 1, slot
    2, a sub-address or anything to FFh, a second command refused while
    it runs, polled per vblank, its buffers) and a level-2 session
    (nothing reserved, both slots' card type through `McxSync(0)`, every
    command to both slots, `McxStopCom` with a transfer running and
    `McxStartCom` timing it out, `McxExecFlag` on the card). Observed per
    vblank: `McxSync(1)`, the command number and result it writes, where
    the guest resumes in the frame (hblanks), the interrupt mask.
    `psxw conform btn --chip memcard`: PASS, 17 case runs (8 seeds):
    slot 1 gives McxErrInvalid and slot 2 McxErrNoCard, two vblanks after
    the call, the start's vblank resuming a quantum (9 hblanks) later on
    both sides. Unit tests over a stand-in object layout
    (`libmc::tests`), the responder's frame read and write, SIO0's card
    slots, the timers' flags and the RAM chain. On the tree merged with
    main (chips cd, mdec and spu): `--chip irq` (33 runs), `gpu` (49),
    `sio0` (34), `cd` (19), `spu` (47) pass in btn, and `irq` (33), `gpu`
    (49), `sio0` (34), `cd` (22), `mdec` (17), `spu` (47) in bof4; `psxw conform btn --lib libpad`
    (125 steps), `--lib libmcrd` (21) and `--lib libetc` (60).
  - **Runs:** btn 1,200 frames, `--check boot --check no-trap --check
    no-shadow-io`: pass native and on `--device memcard` (952,146 `VSync`
    calls both ways; `McxStartCom` once). Over 3,000 frames with no replay
    the two are equal (RAM and VRAM hashed at 6 checkpoints, 1,371,263
    render words, the same 1,698,611,325 cycles); on the device libmcx's
    verifier reads I_STAT/I_MASK 7,325 times, natively no guest code
    touches a register of the chip. With chip spu merged btn builds with
    every chip native and passes `--check no-io` (1,200 frames). bof4 (no
    libmcx): 1,200 frames pass, `title-to-field` passes every expect at
    the same frame (counter 12390 at frame 24768). Level 4 (`psxw conform
    <game> --modes`): bof4 passes, its output (renderer, displays, audio)
    identical in both modes. **btn's replays say nothing here:** chip
    cd's regression since `650da93` stalls btn in loading under any
    replay (`CdRead: sector error` from frame 97, 30 render words), in
    every mode, so `card-load` (its checks, `--check card` included,
    pass) and `title-to-gameplay` never reach the card menu, and level 4
    for btn passes its checks with output that differs between the
    modes. To rerun once the fix lands.

- **2026-10-29, btn's replays stall (task 9d's regression), D66.** Since
  `650da93` btn never left loading under any replay, natively and with
  `--device all`: `CdRead: sector error` / `retry...` from frame 97 on,
  30 render words, the frame counter at 0. Task 9d had missed it (it read
  the last ten log lines, and `title-to-gameplay` had no expect lines).
  - **Cause:** libpad's vblank interrupt busy-waits on SIO0 and root
    counter 2, and under the safepoint model each wait ended at the next
    safepoint: with two pads (every replay) the interrupt took 14 quanta,
    280,000 cycles, longer than a sector period at double speed (225,792).
    The controller replaced the sector the CPU hadn't taken with the next,
    as hardware does, and `CdRead`, original since D62, found each header
    wrong and retried forever. One pad (no replay) took ten quanta and got
    by, which is why task 9d's runs passed. A quantum of 10,000 or 5,000
    cycles made the errors go away (an experiment, reverted); a controller
    that queued unread sectors didn't (libcd acknowledges INT1 before
    reading the sector, so the queue's next one is presented).
  - **Fix (D66):** SIO0 and root counter 2 see `Machine::guest_now`, the
    clock plus the share of the quantum the guest's loop passes have
    taken, so a poll sees the byte, /ACK or count at the cycle it is made.
    libpad's handler and libmcx's address-byte wait count passes the same
    way and spend them from the guest's budget
    (`libpad::spend_in_exception`): about 640 passes (12,800 cycles) for a
    pad and an empty port, 58 for libmcx's wait. SIO0 alone left the
    counter waits at a quantum each: btn played through but logged 231
    sector errors in `title-to-gameplay`; with counter 2, none. Giving
    every counter the guest's cycle moved bof4's native run off the device
    with chips irq and gpu (from frames 735 and 102): root counter 1,
    which `VSync` reads, then depended on the budget that the adapters
    replacing loops don't spend.
  - **Replays:** `title-to-gameplay` has three expect lines (the farm,
    scene 2, at counter 2100; the player facing 3 after the mayor's walk
    at 2600; still on the farm at 2700), which a stalled game can't pass.
    btn's counter word holds 66158592 from frame 76 to 84 before the game
    zeroes it, so an expect is now taken where the counter first equals
    its mark, else where it first went past it and stayed (`psxw run`).
    bof4's `title-to-field` reaches the field about 370 frames later
    (counter 12390 at frame 25,138, was 24,768) and runs 26,000 frames.
  - **Checks, on main after the chip memcard merge (`6b4405d`):** unit
    tests (sdk-psyq, psx-bios, psx-devices, psxw-cli, psx-runtime) pass.
    Natively: btn `title-to-gameplay` passes its three expects (frames
    4,725, 5,725, 5,925; 10,511,025 render words; no sector error), btn
    `card-load` passes `--check card` and gets to the card (`_card_info`
    81 times, `_card_load` 3; no sector error), bof4 `title-to-field`
    passes its five expects. `psxw conform btn --chip cd` (19 runs),
    `--chip sio0` (34), `--chip memcard` (9) and `psxw conform bof4 --chip
    cd` (22): PASS, 4 seeds. Level 4 (`--modes`) wasn't rerun: for btn it
    has the boot GPU timeout's offset between the modes (task 9a's
    finding, frames 64 and 99).

- **2026-10-29, phase 1 closes** (main at ac60058). Every chip runs
  native in both games: `psxw build` reports "emulated: none" for btn
  (cd, gpu, irq, memcard, sio0, spu) and bof4 (cd, gpu, irq, mdec, sio0,
  spu), and both `game.toml` files expect it. btn binds 86 adapters, bof4
  92, 96 between them (BIOS-call and BIOS-internals ones included).
  The state of each exit criterion, with the last run that backs it:
  1. **Same code everywhere:** `psxw build` refuses a runtime with two
     adapters of one name; sdk-psyq's `agnostic` test fails on non-test
     adapter code that names a game or holds a guest RAM address (it
     catches a planted one). Passing at ac60058.
  2. **Conformance, levels 1–3:** no adapter is on `OWED`. Every chip's
     cases passed in each game that links it on the merged tree before
     D66 (c02c4a6 plus chip memcard): irq 33, gpu 49, sio0 34, cd 19/22,
     spu 47, mdec 17 (bof4), memcard 9 (btn). After D66 (ac60058) cd,
     sio0 and memcard were rerun and pass; irq, gpu, spu and mdec weren't.
  3. **Both games native:** as above.
  4. **Fallback:** every chip's `--device <chip>` build boots both games
     (each chip's task). Level 4 passed for bof4 at c02c4a6 with the
     renderer commands, displays and audio identical in both modes over
     25,300 frames. btn's level 4 never ran clean before the D66 fix, and
     neither game's was rerun after it: the user judged the runs too
     costly for this round. `psxw conform <game> --modes` runs it.
  5. **`psxw ci btn` and `psxw ci bof4`:** not run (the user's call: they
     take hours). The new gates in it are `conform` (now with every
     native chip's cases, and for bof4 too), `sdk-reach` and `fallback`.
  Native replays at ac60058: btn `title-to-gameplay` (its three new
  expects) and `card-load --check card`, bof4 `title-to-field` (its five
  expects; the field now arrives at frame 25,138, so the replay runs
  26,000 frames).

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
  it: exit criterion 4 needs it. *Task 9d called it resolved, wrongly:*
  it checked runs without a replay, which connect one pad. Under a
  replay (two pads) it was the stall below, and since task 9d, with
  btn's `CdRead` original in every mode, it stalled natively too.
  *Resolved by D66* (progress log, "btn's replays stall").
- **Conformance ran every case in one calling context** (found in the
  browser, 2026-10-29). btn's in-game menu keeps its stack in the
  scratchpad. libetc's adapter ran handlers on the interrupted code's
  stack, where the original switches to `intr.o`'s own, so libcd's CdRead
  callback took each sector header by DMA into the scratchpad, which DMA
  doesn't reach, and retried forever ("CdRead: sector error"). Fixed in
  7343c6a. No case caught it: the harness always calls from main code on
  the game's boot stack, and stacks are outside every RAM comparison;
  level 4's replays never open the menu. The README now makes calling
  contexts part of conformance, and phase 2's task 0 gives the harness
  them; phase 1's adapters are checked in the first context only until
  then.
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
- **libpad's interrupt takes half a frame under the safepoint model**
  (task 9c). Each of the original's busy-waits on SIO0 or root counter 2
  ends at the next safepoint, a quantum (20,000 cycles) later, inside the
  vblank interrupt: 7 quanta for a polled digital pad, 5 for a refused
  config-mode command, 3 for an empty port. With two pads that is 280,000
  cycles a vblank, half an NTSC frame, where hardware takes about 20,000
  for both ports. Games see it (bof4's main loop drops to half rate in
  places), and the replays were authored over it. A first adapter that
  spent no time put bof4's `title-to-field` 322 frames behind the device
  (counter 12390 at frame 25090) and missed its last two expects.
  Decision (D60, as for `VSync` in task 9a): the adapters spend the
  original's quanta. Open, like the `VSync` finding: the device timing is
  an artifact of the time model; a finer quantum would change both modes
  alike.
- **The guest's safepoint budget is observable too** (task 9c). With the
  quanta spent but the budget left as it was, bof4 took another path from
  frame 110 (its expects read 0): the original's last wait leaves the
  budget full, so the game does a quantum's worth of iterations before
  time moves again; left alone, it does whatever was left. Starting the
  budget over makes both replays equal to the device frame for frame;
  starting it one or two iterations short made 648 and 1,331 of btn's
  6,000 frames differ (btn's polling loops count iterations: libgpu's GPU
  timeout counts `VSync(-1)` polls). `VSync`'s adapter (task 9a) leaves
  the budget as it was; that didn't show because libpad's interrupt, on
  the device then and native now, starts it over at every vblank, just
  before `VSync` returns. Open for chip irq: a game that polls no pad
  would see it.
- **A digital pad only, on both sides.** SIO0's device (Beetle's
  `InputDevice_Gamepad`) and the adapters' wire model answer a poll and
  nothing else, so libpad's extended paths (config mode 43h, actuator
  alignment 4Dh, the info tables, DualShock IDs) are unexercised:
  `PadSetActAlign` always reports the engine busy, `PadInfoMode`'s ID
  table is empty, the actuator bytes libpad sends after the command
  aren't computed. A DualShock model in the device would need the same
  in the adapters' wire, and the engine's command states past 1.
- **The scan's reach for libpad is its function-pointer table.**
  `PadGetState`, `PadInfoMode` and `PadSetAct` touch no register; they
  call through `_padFuncPort2Info`, so the scan gives them everything
  libpad's pointers can reach (DMA, IRQ, SIO0, exp2, timers). Adapting
  them is what makes the chip native, and they read state the engine
  keeps in the runtime anyway.
- **`PadSetAct`'s result is libpad's record address** (v0 left by the
  `_padFuncPort2Info` call); the adapter returns 0 and the cases don't
  observe it. `PadInitDirect` and `PadStartCom` leave 1 in v0,
  `PadStopCom` ExitCriticalSection's 0; the `--lib libpad` scenario
  observes those, and the adapters return them.
- **libgpu's state stays in guest RAM** (task 9b, D61). Keeping it in the
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
- **The CD's seam is its drive layer** (task 9d, D62): decided without
  the human's input, as the task said. See the progress log.
- **btn's `CdRead` never got a sector through under a replay** (task 9d's
  regression; D66). Task 9d saw only the last ten log lines of
  `title-to-gameplay` and called five sector errors late in the run
  benign; in fact `CdRead: sector error` / `retry` repeated from frame 97
  on, every replay stalled in loading (30 render words, counter 0), and
  the replay passed because it had no expect lines. It has three now.
- **Run reports' `cd_loads` are empty now.** The old `CdRead` adapter
  timed each load (M7 task 7's evidence); with `CdRead` original nothing
  sees a load whole. Sectors and EDC are still counted (the controller's).
- **Stand-in addresses share dispatch slots with RAM.** The table indexes
  `addr & 0x3F_FFFC`, so libetc's, libcd's and libgpu's stand-ins
  (`0xBFC7_0010` … `0xBFC7_0100`) share slots with `0x8007_0010` …: a
  game with a function exactly there would never reach the stand-in.
  Neither game has one (checked in both games' tiers).
- **bof4's map calls `CdReadyCallback` `CdSetDebug`** (`libcd/s_015`; the
  signatures name it `CdReadyCallback`, the two have the same code). Cases
  resolve functions by name, so bof4's level-1 ring case calls
  `StCdInterrupt` itself and its callbacks come through libds in
  `cd.stream`.
- **What the cd adapters don't reproduce, deliberately.** The debug
  printouts (`CD_debug`; only `CD_init`'s address line is logged), the
  memory controller's delay writes around transfers, the handler's
  address (`CD_initintr` returns the stand-in natively, `bios_1`'s
  routine on the device: cases observe set or not), v0 left by the void
  `StCdInterrupt`, and the original's hang when `StCdInterrupt` finds the
  sector buffer empty (it waits for data that never comes; the adapter
  moves on). `CD_initvol` writes the SPU's registers with the device's
  semantics until chip spu has adapters.
- **Late interrupt service and libds** (task 9a's open question): chip cd
  doesn't change it. IRQ2 is taken at the same safepoints natively and on
  the device, from one controller, and bof4's replay reaches every expect
  at the same frame both ways. Whether hardware's prompt service sends
  bof4's intro elsewhere still needs the Beetle oracle (not run here).
- **btn with chip irq on its device and chip gpu native fails
  `no-shadow-io`** (found by task 9d on the tree merged with main): `psxw
  build btn --device irq`, 1,200 frames: 16 bytes of `0xFF` reach the I/O
  window's shadow at `0x1F801C44` (the SPU's voice registers) at frame 65,
  just after libgpu's timeout reset. The same with the CD on its device
  too (`--device irq,cd`) and with SIO0 (`--device irq,sio0`); not with
  the GPU on its device as well (`--device irq,gpu`), and not in bof4. So
  it is chip gpu's adapters under libetc's own dispatcher, not the CD's;
  left to chip gpu's owner. *Resolved (progress log, exit criterion 4):*
  it wasn't chip gpu. The bytes are SPU registers that lifted libspu
  (`_spu_init`) wrote with one merged 8-byte store, and the runtime's
  `io_access` passed only the first word to the SPU. It happened in every
  mode and both games. The check only compares the shadow at vblanks, and
  only btn on `--device irq` (with or without the CD or SIO0) put a vblank
  between the store and the next accesses to those registers.
  `--device irq,gpu` changes the boot's pace, and no vblank fell there. A `--debug-io` build didn't trap because the store is
  a device library's and goes to `io_access`, not `io_trap`. The bus now
  takes each access as the aligned registers it covers.
- **clang changes the shape of a device library's register accesses.**
  Lifted code's loads and stores are plain C (only loop heads have a
  barrier), and clang merges neighbours, so a device can get one wide or
  unaligned access where the original made several narrow ones. The bus
  splits an access into aligned registers in address order. That is the
  original's order when its stores ascend, as `_spu_init`'s do. A merge of
  stores the original made in another order would reach the device in
  address order. That hasn't been seen. Counted by width over 1,200
  frames on `--device all`, the only accesses wider than a word in either
  game are SPU stores, no access is unaligned, and no load is wider than
  a word.
- **An adapter's interrupt is taken at its own return; the original's at
  the guest's next safepoint** (task 9e, D64). A first set of libpress
  adapters raised IRQ3 for DMA1's completion as the device does, and
  `driver::run` took it as the adapter returned, so the slice callbacks
  (`LoadImage` of each 16-pixel column, the next `DecDCTout`) ran before
  bof4's player went on. On the device the original's register write
  raises it inside guest code, and the player first decodes the next
  frame's run-length codes (`DecDCTvlc2`, called right after
  `DecDCTout`) up to a safepoint. From the first decoded frame (1,198) a
  word the player updates each frame, `0x80116BB8`, read 10 less natively
  (`0x138F` against `0x1399`; as hblanks, about a safepoint quantum); the
  movie, the frame counter and the expects were the same. Holding the
  interrupt until the adapter returns (`Machine::hold_irq`) removed it.
  Open for chip gpu: its adapters' DMA2 completions are still taken at
  their return, which may be what its finding on bof4's DrawSync-measured
  draw time (9 hblanks less natively) sees; `dma_done_held` would apply.
- **Dead stack under the movie** (task 9e). With the interrupt held, RAM
  during the movie differs from `--device mdec` only below the stack:
  `0x801FF73A`, `0x801FF762` and `0x801FFF3A` hold `0x10` on the device
  (the high half of libpress's wait counter, `0x100000` at `16(sp)`), and
  `0x801FF768` `0x80` natively. Not reproduced, as libgpu's frames
  aren't; by frame 3,000 RAM is equal.
- **The device's DMA1 doesn't wait for an idle MDEC.** A `DecDCTout`
  with no decode in progress takes what the output holds and zeros past
  it, at once; hardware would wait for the MDEC's data-out request. The
  adapters reproduce the device (principle 6): `DecDCTout` before its
  `DecDCTin` reads zeros unless the MDEC is still taking an earlier
  command's input. A DMA count of zero blocks is one block on both sides
  (the controller's `words()`). bof4 always calls `DecDCTin` first.
- **Where bof4's movie is.** In `title-to-field` the first libpress call
  (`DecDCTReset(0)`) comes between frames 1,150 and 1,190, the first
  decode at frame 1,198 (counter 762), the last near 2,000; the frames
  780-850 the task named are the counter's range, not the replay's.
  The player's out callback calls `StCdInterrupt` first when libcd's ring
  deferred its interrupt, so chip cd's ring adapter runs inside DMA1's
  callback.
- **Chip cases and overlays.** bof4 places libpress in an overlay
  (`ovl_4414`, `0x801CC800`-`0x801E02A0`), which a fresh conformance
  machine doesn't have. The chip harness writes the overlays a case's
  functions resolve into before it starts (§3: the bytes in RAM decide),
  and fails if one lies over the cases' scratch RAM (`SCRATCH`, now with
  `SCRATCH_LEN`, 48 KiB) or over another the case needs.
- **libspu's seam is below the scan's** (task 9f). The scan's set is the
  public functions the games and libsnd call (18 of libspu, `SsInit`);
  the build's chip decision, with libspu's state left in its `.data`,
  needs only the functions whose own code reaches the SPU: 12 of those
  18 and 8 internal ones (`spu.o`'s layer, `_SpuSetAnyVoice`,
  `_SpuGetAnyVoice`, `SpuSetNoiseClock`, which libsnd's `vmNoiseOn`
  calls), plus `_SsInit`. The other 6 public ones (`SpuWritePartly`,
  `SpuSetTransferStartAddr`, the noise and reverb voice wrappers) run
  original over them, as `SsInit`, `SpuInit` and the accessors do.
  `SpuClearReverbWorkArea` reaches the SPU only through `_spu_t`, but is
  adapted: the reach table lists it, and its loop waits on `WaitEvent`.
- **Budget passes are the translation's, not the source's** (task 9f).
  Counted from the MIPS loops, the first adapters spent 22 passes too
  many in `SpuInit` and over 500 in `_SsInit`, and conformance showed it
  at once (the budget left is observed after every call). Measured
  against the original over the device: clang unrolls the constant loops
  of 10 and 16 iterations (`_spu_init`'s register queue, `_SsInit`'s
  registers and its mark table's inner loop), `_spu_init`'s 24 voices go
  two a pass, `SpuSetReverbModeParam` copies its 68 bytes a word a pass,
  `SpuGetKeyStatus`'s search for the lowest bit is a count of trailing
  zeros (no pass), and `SpuClearReverbWorkArea`'s chunk loop passes once
  per chunk after the first. 24-voice loops elsewhere stay one pass per
  voice. A toolchain change that moves these shows in `--chip spu`; the
  constants are in `libspu.rs`.
- **An adapter's return is a safepoint the original doesn't have**
  (task 9f). With DMA4's completion held (D64), btn still made one more
  `VSync` poll than `--device spu` from frame 395: `SpuWritePartly` (guest
  code) calls `_spu_Fw`, then `_spu_FsetRXXa`; natively both are
  adapters, so the held IRQ3, released at `_spu_Fw`'s return, ran at
  `_spu_FsetRXXa`'s, inside `SpuWritePartly`, and the game's poll of
  `SpuIsTransferCompleted` saw the transfer done one call early. libspu's
  adapters now return without a safepoint (`Machine::quiet`), as their
  originals do; `VSync` counts and the frame counters match the device in
  both games. The other chips' adapters still drain at their return;
  where their originals make no runtime call at the end, the same rule
  would apply (open).
- **The register file keeps what was written.** `_spu_init`'s hot path
  writes KOFF's high half as 0xFFFF (only its low 8 bits latch), and the
  register reads back 0xFFFF; a first adapter wrote 0xFF and the SPU's
  state hash differed. The mixer API writes each half as the original
  does. Before main's wide-store fix (`5a3a920`) the device's `_spu_init`
  lost the pitch and start address of every voice (8-byte stores cut to
  4); the audio didn't show it (the voices are silent), the state hash
  would have.
- **What the spu adapters don't reproduce, deliberately.** libspu's
  waits that would time out (a status bit the core never leaves set)
  spend their 3,840 polls at once; RAM a callback reads during a
  safepoint inside a RAM-only loop (`_SsInit`'s mark table) is written
  whole, not halfway; the stack libspu's frames leave below `sp` (5 bytes
  in btn's 1,200 frames, 13 in bof4's). Register-queue mode
  (`_spu_env`) is followed, but no game here links `SpuSetEnv` or
  `SpuFlush`. The M6 SPU recording hook (`TRACE_RECORD_SPU`) sees
  register writes, so natively a window ends when an adapter touches the
  SPU; record with `--device spu`.
- **btn's `title-to-gameplay` is silent** on both sides (0 of 4.4 million
  audio frames audible, natively and on `--device spu`, its first 1,200
  frames included), while the same 1,200 frames without a replay (no
  pads plugged) are half audible. Not chased (it predates chip spu and
  is the same on the device); the replay comparison with sound is
  bof4's `title-to-field` (17 million audible frames).
- **libmcx is a chip, memcard, not BIOS internals** (task 9g, D65). It
  drives SIO0, IRQ7 and root counter 2 itself; its acts on the kernel
  are its element, appended to priority 2's chain directly through the
  table of tables at 100h (so original libmcx registered nothing under
  the HLE kernel, whose chains were Rust lists), and a patch of the
  exception vector its bulk path takes only when entered from there.
  With the kernel's blocks in RAM, original libmcx runs over SIO0, the
  oracle the adapters are compared against. A separate chip from sio0:
  its devices are the slots' cards, not the pads, and sio0's mode in both
  games needn't follow libmcx's in btn.
- **The root counters lacked mode bits 11 and 12** (found by the memcard
  cases). `_mcxChkRC2wait` adds a wrap's 10000h when the mode says the
  counter reached FFFFh (or the target, when it resets there), which a
  mode read clears; libpad's own wait adds the target instead and never
  needed them. Without the bits the device's wait for 1,160 cycles lasted
  from 1 to 23 quanta by root counter 2's phase (and could last for
  ever), against 1 natively: the cases saw the guest resume at hblank 92
  or 213 instead of 9. `Timer::read_mode` now reports and clears them.
- **SIO0's /ACK interrupt is taken at the next interrupt the machine
  raises**, not at the next safepoint: the device raises I_STAT's bit as
  it is brought up to date at a register access, and nothing walks the
  chains until another line (a vblank, a root counter) rises. The
  adapters do the same (the /ACK is an event that only raises the line).
  libmcx's bytes after the address therefore go one per interrupt, and
  its vblank handler times out a transfer still running: a standard card
  answers `McxExecFlag`'s `R` and `W`, but the transfer (140 bytes) ends
  in McxErrInvalid in both modes, where hardware would read the block's
  directory frame and write it back with the flag (and a new card's FLAG
  would give McxErrNewCard). Open, with the safepoint model: hardware
  takes each /ACK within microseconds.
- **Where btn calls libmcx, and what showed it.** btn calls `McxStartCom`
  at boot; `McxCardType` with `McxSync(0)` comes from the card checks in
  `ovl_31` (after `MemCardAccept`, for two of its results) and `ovl_15`,
  and every command from `ovl_10`, a PocketStation menu. The cases are
  what exercise the transfers: under the chip cd regression btn's
  replays stall before the title (progress log), so `card-load` called
  no libmcx function but `McxStartCom`. Open: rerun `card-load` and
  `title-to-gameplay` (and btn's level 4) once that fix lands; on the
  way to its "No loadable diary" message, if `card-load` passes
  `ovl_31`'s check, slot 1 answers McxErrInvalid two vblanks later in
  both modes.
- **What the old libmcx adapters got wrong.** They answered at once,
  where the original's answer comes at the second vblank (a game's
  `McxSync(0)` waits that long); `McxSync` wrote 1 as the command number
  (the original writes the command's, 13h for `McxCardType`) and left
  its out-parameters alone when nothing was reserved (the original
  zeroes them); `McxStartCom` did nothing (no vblank acknowledge or mask,
  no `ChangeClearRCnt(3, 0)`).
- **`hardware_unbound` sees libmcx's constant-address internals only**
  (`_mcxSetRC2wait`, `_mcxChkRC2wait`, `_mcxSioRW`, `_mcxFastDsrEnt`, all
  unreachable): it uses the translator's facts, so the internals that
  reach SIO0 through libmcx's register pointers (`_mcxInitSioMode`,
  `_mcxStepSio`, the handler, the verifiers) aren't listed. Their
  unreachability is the chip decision's, which uses the reach table and
  reports memcard native.
- **`_mcxStartFastDsr` runs as guest code in both modes.** An Rx callback
  calls it when a PocketStation's length arrives (never without one): it
  asks the BIOS for the C0h table (0x674 here, zeros), writes its vector
  stub at entry 6 + 70h, so at 70h, and flushes the cache. Harmless in
  RAM the HLE BIOS doesn't use; the bulk bytes then reach
  `_mcxFastDsrEnt` from `_mcxDsrEntry` (`_mcxCallFromC`), which the
  adapter reproduces.
- **A PocketStation isn't modelled**, so libmcx's success paths (a
  command's reply, `_mcxStepSio`'s last-byte read, the bulk transfer of
  `ReadDev`/`WriteDev`/`SetUIFS`) are implemented from the objects but
  unexercised on either side, as libpad's DualShock paths are.
- **When SIO0's cards are loaded is observable.** A first version loaded
  the BIOS's images at SIO0's first access, so libpad's original code on
  the device (`psxw conform btn --lib libpad`) loaded a fresh card and the
  BIOS wrote it back at the next vblank, which the scenario saw and the
  adapter side didn't. The cards load when a card's address (81h) first
  goes out, on the device and natively alike; a pad driver never sends
  it.
- **Instant CD timing is no seek time, nothing more** (360621c). With
  sectors taking no time a read delivered the whole disc in one
  `Cdc::run` (btn: out of memory at frame 65, in `CdSearchFile`); pacing
  sectors by the CPU's acknowledge instead broke libcd's `CdRead` (it
  sets its count after issuing ReadN, and waits 0x79 frames for the
  closing Pause) and libds (it takes a sector in its DMA callback before
  the next INT1). So instant keeps the drive's sector rate and command
  latency: at 600 frames btn renders 141,683 words (realistic 128,620)
  and bof4 844,850, no CD errors, realistic unchanged.
