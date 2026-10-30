# Unified adapter layer

Shared context for [phase 1](phase1.md) (every adapter btn and bof4 need)
and [phase 2](phase2.md) (the rest of PsyQ 4.7).

**Design refs:** [docs/design.md](../../docs/design.md) §5 (the SDK
boundary), §6 (time, interrupts); decisions D4, D40–D42, D45, D47, D58.
Background: [docs/emulation-layers.md](../../docs/emulation-layers.md),
[docs/status-btn-vs-bof4.md](../../docs/status-btn-vs-bof4.md).

## Goal

Each chip runs native, through SDK adapters, wherever a validated adapter
set covers everything a game uses on it. Otherwise the chip falls back to
its register-level device model (graceful degradation). Every build reports
how much of the machine was emulated, and why.

## Why

The CPU is not emulated: everything is compiled ahead of time. Below the
SDK, D40–D58 moved the boundary to the hardware registers. The original
library code runs against register-level models of the interrupt
controller, timers, DMA, GPU, SPU, SIO0, CD controller and MDEC. That
bought exactness by construction and SDK-version independence, and it made
the layer beneath the SDK an emulator.

| Chip | btn today | bof4 today |
|---|---|---|
| CD | native (libcd adapters) | emulated |
| GPU, SPU, SIO0, IRQ, timers, DMA | emulated | emulated |
| MDEC | — | emulated |
| Memory card | native (BIOS) | native (BIOS) |
| GTE | emulated, by design | emulated, by design |

## Principles

1. **One adapter per SDK function, the same code in every game.** No
   per-game variants, forks or modes. If an adapter works in one game but
   not another, widen the seam or the shared state. Never add a second
   adapter.
2. **Per chip, one state model.** All adapters for a chip share one state:
   one drive, one GPU queue, one mixer, one pad source. Whatever library
   code a game keeps around them (libds, streaming, libsnd, libgs) lands on
   that same state.
3. **Native or emulated is decided per chip.** A chip can't have two
   drivers. If anything a game links reaches the chip outside the adapter
   set, the whole chip falls back to its device.
4. **Two kinds of adapter.**

   | Kind | Replaces | Falls back? |
   |---|---|---|
   | Hardware | SDK code that drives a chip | Yes: to the device model |
   | BIOS internals | SDK code that patches or reads the Sony BIOS (libcard's `_patch_card`, libapi's pad flags) | No: there is no Sony BIOS to emulate. Mandatory. |

5. **Adapters reproduce observable behaviour, not code.** Decompiled C
   helps where an adapter must keep internal state consistent with library
   code that stays original. It never gates an adapter: the disassembly,
   our translated C and the A/B oracle cover the rest.
6. **No hardware in this plan.** The oracle is the original code over our
   device models. Where the chip decides the outcome, that is provisional,
   and reports say so.

## Vocabulary

| Term | Meaning |
|---|---|
| **Seam** | The set of functions a chip's adapters replace. Everything above it runs as original code. |
| **Chip group** | The libraries that drive one chip: CD (libcd, libds, streaming), SPU (libspu, libsnd), GPU (libgpu), SIO0 (libpad), IRQ and timers (libetc, libapi's counters), MDEC (libpress). |
| **Reach hardware** | A function whose library's own code touches a register: directly, through calls, or through function pointers. |
| **State accessor** | A function that touches no register but shares data with functions that do (callback setters, mode getters, flags). |
| **Needed set** | A game's linked functions that reach hardware or are state accessors, in an adapted library. |
| **Adapter set** | The needed functions that code staying original calls: the game, or SDK code outside the needed set. The internals behind them need no adapter. |

## The hardware-reach analysis

Which functions need adapters comes from a static analysis of the SDK's
relocatable library objects (psyq-decomp `library-input/redux`, PsyQ 4.7).
It follows:
- register pointers held in data, including pointers stored at run time;
- loads and stores through constants or those pointers;
- calls, by relocation, across libraries;
- calls through function-pointer tables;
- data shared with hardware-reaching functions (state accessors).

Per game, it joins that with the functions the build links
(`out/<game>/tiers.json`) and with what the game's code calls.

A prototype produced the numbers in these plans. It isn't in the repo.
Phase 1 task 1 builds it as a `psxw` subcommand.

**What the prototype found:**
- **libds touches no register itself.** It drives the CD through 11 of
  libcd's low-level functions (`CD_cw`, `CD_sync`, `CD_ready`, `CD_init`,
  `CD_getsector`, `CD_getsector2`, `CD_datasync`, `CD_flush`, `CD_initvol`,
  `CD_vol`, `StCdInterrupt`). libcd's public API sits on the same layer.
- **libsnd touches the SPU in 3 functions.** The rest goes through 23 of
  libspu's.
- **libgs touches no register.** It goes through libgpu and libapi's
  counters.
- **Indirect calls matter.** libgpu's API reaches the GPU mostly through
  its function-pointer table: 13 functions by direct calls alone, 36 with
  indirect calls.
- **It checks out against what we've written.** Of the 82 adapters we
  have (libcd and libapi today, plus the old libgpu, libetc and libpad
  sets), it finds 60. The other 22 are state accessors, which led to the
  state-coupling rule.

## Native backends

Adapters call the cores the device models already contain, without the
register protocol around them.

| Chip | Core | Status |
|---|---|---|
| GPU | packet decoder (`psx_devices::gpu`) and renderer (`psx-render-gl`) | ✓ |
| CD | disc image, ISO9660, drive timing (`psx_devices::cd`), driven through the CD controller's core (`psx_devices::cdc`, phase 1 task 9d) | ✓ |
| XA audio | decoder (`psx_devices::xa`) | ✓ the controller decodes it into the SPU's CD input, natively as on the device (task 9d) |
| SPU | voice DSP (`psx-spu`) | ✗ register API only; needs a mixer API |
| MDEC | decode core inside `psx_devices::mdec` | ✓ `Mdec::input`/`output`, shared by DMA0/DMA1 and libpress's adapters (9e) |
| SIO0 | host pad state | ✓ |
| IRQ, timers, vblank | virtual clock, guest callback queue | ✓ |
| Memory card | card images through the BIOS (`psx-bios::card`); on the wire a standard card's answers over them (`psx_devices::memcard::Responder`), which SIO0's device and libmcx's adapters share | ✓ |

## Conformance

An adapter is done when it matches the original library code running over
the device models, on every observation that code staying original can
make. `psxw conform` already does this for six libraries, with scenarios
fixed to btn's addresses. The suite has four levels:

| Level | Compares | Inputs |
|---|---|---|
| 1. Per adapter | One call: adapter vs the original over the devices | Seeded random arguments within each function's preconditions, like `psxw difftest` |
| 2. Per chip, in sequence | Init → use → sync → callbacks, across a chip's adapters, including calls from library code that stays original (libds into libcd, libsnd into libspu) | Hand-written sequences |
| 3. Every game that links it | Levels 1 and 2 in each game, functions resolved by symbol, not address | The same cases. This level enforces principle 1. |
| 4. Whole game | A replay in adapter mode vs device mode: frames and RAM at counter checkpoints | The games' replays |

**Observation specs** live next to each adapter:

| Observation | How it's compared |
|---|---|
| Return value, out-parameters | Exactly |
| Device effect | At the core's API, which both paths reach: the GPU's render commands, the SPU's voice state, sectors in RAM, pad buffers, decoded pixels |
| Library globals | Only those that code staying original reads |
| Callbacks | Order and arguments exactly; timing to the frame |
| Timing | At sync points (after a `*Sync` returns, after N vblanks), not per cycle |

## Build reporting

- **Per chip:** native or emulated, and the reason. Possible reasons: an
  unvalidated object, code that reaches past the adapters (and where), or
  no adapters for the chip.
- **Summary:** one line per build ("emulated: CD, MDEC; 2 of 7 chips"),
  also written to `build.json`.
- **At run time:** register accesses per chip in the run report, and
  `--check no-io` for builds where every chip is native.
- **Expected chips:** `game.toml` lists the chips expected to be native. A
  build fails if one falls back, so degradation is never silent.

## Decisions this plan needs

Phase 1 starts by recording D59 in the design's decision log: adapter
first, device fallback, per chip. It reverses the "device" default of D40,
D42, D45 and D47, and generalizes D41 and D58 (libspu's and libcd's
adapter-or-device policies).
