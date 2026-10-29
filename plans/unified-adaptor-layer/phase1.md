# Unified adapter layer, phase 1: btn and bof4

**Status:** not started
**Depends on:** M13 (bof4 boots and plays through `title-to-field`).
**Unlocks:** [phase 2](phase2.md).
**Shared context:** [README.md](README.md): principles, vocabulary,
conformance and reporting.

## Goal

Every adapter btn and bof4 need exists, is the same code in both games,
and passes conformance. Both games then run every chip native except the
GTE. The device models stay, as the fallback and as the oracle.

## Inventory

From the prototype analysis (PsyQ 4.7; btn `b37ae28`, bof4 `1560817`).
Task 1 regenerates these numbers with the Rust analysis.

**121 adapters** across both games: btn needs 95, bof4 76, and 50 are
shared. 22 exist. About 45 of the 121 are state accessors, a rule that
over-counts (task 1 refines it). The mandatory BIOS-internals adapters
(libcard; libapi's pad and counter functions) aren't counted; they exist.

| Library | Need | btn | bof4 | Have | Decompiled (psyq-decomp HEAD / our pin) |
|---|---|---|---|---|---|
| libspu | 26 | 23 | 23 | 0 | 3 / 3 |
| libgpu | 27 | 24 | 15 | 0 | 24 / 24 |
| libcd | 24 | 11 | 13 | 11 | 18 / 18 |
| libmcx | 19 | 19 | 0 | 4 | 12 / 0 |
| libetc | 8 | 6 | 8 | 0 | 7 / 7 |
| libapi | 7 | 5 | 7 | 7 | 5 / 5 |
| libpad | 6 | 6 | 6 | 0 | 6 / 0 |
| libpress | 3 | 0 | 3 | 0 | 3 / 0 |
| libsnd | 1 | 1 | 1 | 0 | 0 / 0 |
| **Total** | **121** | **95** | **76** | **22** | **78 / 57** |

psyq-decomp HEAD is `935cc39`; our pin `302b6ae` is 832 commits behind it.

**libcd is the unification problem in one table.** btn calls the public
layer (`CdControl`, `CdRead`, `CdSync`, …). bof4 calls the low layer
(`CD_cw`, `CD_sync`, …) through libds, plus streaming (`St*`). The two
games share no libcd adapter today.

### The functions

| Library | Reach hardware | State accessors |
|---|---|---|
| libspu | SpuClearReverbWorkArea, SpuGetAllKeysStatus, SpuGetKeyStatus, SpuGetNoiseVoice, SpuGetReverbVoice, SpuGetVoiceEnvelope, SpuGetVoiceVolume, SpuSetCommonAttr, SpuSetKey, SpuSetNoiseClock, SpuSetNoiseVoice, SpuSetReverb, SpuSetReverbDepth, SpuSetReverbModeParam, SpuSetReverbVoice, SpuSetTransferStartAddr, SpuSetVoiceAttr, SpuSetVoiceVolume, SpuWritePartly | SpuFree, SpuInitMalloc, SpuIsTransferCompleted, SpuMalloc, SpuSetTransferMode, `_spu_getInTransfer`, `_spu_setInTransfer` |
| libgpu | ClearImage, ClearOTagR, DrawOTag, DrawOTagEnv, DrawSync, FntFlush, FntLoad, KanjiFntFlush, KanjiFntOpen, LoadImage, MoveImage, MoveImage2, PutDispEnv, PutDrawEnv, ResetGraph, SetDispMask, StoreImage | ClearOTag, DrawSyncCallback, FntOpen, FntPrint, KanjiFntClose, KanjiFntPrint, SetDumpFnt, SetGraphDebug, two statics in `4-sys.o` |
| libcd | CD_cw, CD_flush, CD_init, CD_initvol, CD_sync, CdControl, CdControlB, CdControlF, CdFlush, CdGetSector, CdInit, CdMix, CdRead, CdReadBreak, CdReadSync, CdSearchFile, CdSync, StCdInterrupt, StUnSetRing | CdLastCom, StFreeRing, StGetNext, StSetRing, StSetStream |
| libmcx | McxStartCom | McxAllInfo, McxCurrCtrl, McxExecApl, McxExecFlag, McxFlashAcs, McxGetApl, McxGetMcxInfo, McxGetMem, McxGetSerial, McxGetTime, McxHideTrans, McxReadDev, McxSetLED, McxSetMem, McxShowTrans, McxSync, McxWriteDev, `_mcxStopFastDsr` |
| libetc | DMACallback, InterruptCallback, ResetCallback, SetIntrMask, StopCallback, VSync, VSyncCallback, VSyncCallbacks | — |
| libapi | ResetRCnt, SetRCnt, firstfile | DisablePAD, EnablePAD, ReadInitPadFlag, SetInitPadFlag |
| libpad | PadGetState, PadInfoMode, PadSetAct, PadSetActAlign, PadStartCom | PadInitDirect |
| libpress | DecDCTReset, DecDCTin, DecDCTout | — |
| libsnd | SsInit | — |

Not decompiled at psyq-decomp HEAD, and no adapter yet (37):
- **libspu (23):** all of its column except SpuGetNoiseVoice,
  SpuGetReverbVoice and `_spu_getInTransfer`.
- **libmcx (6):** McxGetMcxInfo, McxGetMem, McxGetTime, McxReadDev,
  McxWriteDev, `_mcxStopFastDsr`.
- **libgpu (3):** PutDispEnv and the two statics.
- **libcd (3):** CD_cw, CD_sync, CdGetSector.
- **libetc (1):** SetIntrMask.
- **libsnd (1):** SsInit.

## Tasks

1. [ ] **Record D59** in [docs/design.md](../../docs/design.md)'s
   decision log (README, "Decisions this plan needs"), and update §5.
2. [ ] **`psxw sdk scan`: the hardware-reach analysis, in Rust.**
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
3. [ ] **Chip groups and the per-chip decision.**
   - `boundary/psyq.toml` names each chip's libraries.
   - `psxw-build` generalizes `adapter_or_device()` from one library to a
     chip group: a chip is native only if every placed object in its group
     that defines an adapter function is validated, and nothing outside the
     adapter set reaches the chip.
   - Expected chips go in `game.toml`.
4. [ ] **Reporting.** Per chip, native or emulated and why, in
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

## Findings
