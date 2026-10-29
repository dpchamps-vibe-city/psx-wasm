# Unified adapter layer, phase 1: btn and bof4

**Status:** in progress (tasks 1–4 done)
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
chip or BIOS internals; 63 are state accessors. 23 exist.

| Library | Reach hardware | State accessors | Have |
|---|---|---|---|
| libapi | 3 | 2 | 5 |
| libcard | 2 (BIOS internals) | 0 | 2 |
| libcd | 20 | 5 | 12 |
| libetc | 8 | 0 | 0 |
| libgpu | 17 | 12 | 0 |
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
   | a | IRQ, timers, vblank | libetc, libapi | 8 + 7 (7 exist) | Old libetc set in `111e045^`. Must coexist with D45's exception path for code that hooks interrupts itself. |
   | b | GPU | libgpu | 27 | Old set (24) in `02180f4^`. libgs rides on it. |
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
