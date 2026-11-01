# SDK adapters: status

**Status:** btn and bof4 run every chip native except the GTE. The rest of
PsyQ 4.7 has no adapters (old "phase 2", not started). The rules are D59 in
[design.md](../docs/design.md); the history and the full conformance spec
are in [`completed/unified-adaptor-layer/`](completed/unified-adaptor-layer/README.md).
How the layers fit together:
[emulation-layers.md](../docs/emulation-layers.md) and
[adaptor-layer-status.md](../docs/adaptor-layer-status.md).

## Current coverage

From the local `out/<game>/build.json` (2026-10-31) and
`psxw sdk scan --check`:

| Chip | btn | bof4 |
|---|---|---|
| CD (libcd drive layer, streaming interrupt) | native | native |
| GPU (libgpu) | native | native |
| SPU (libspu, libsnd's `_SsInit`) | native | native |
| SIO0 (libpad) | native | native |
| IRQ, timers, DMA (libetc, libapi counters) | native | native |
| MDEC (libpress) | not linked | native |
| PocketStation card port (libmcx) | native | not linked |
| GTE | Rust model, by design | Rust model, by design |

- Both builds report `emulated: none; 0 of 6 chips`, and `game.toml`
  requires that, so a fallback is a build error.
- Adapter-tier functions: btn 86, bof4 91.
- Library code above each seam (libcd's public layer, libds, the libsnd
  sequencer, the rest of libspu) runs as original code over the adapters.
  The scan's per-library "Missing" column lists those functions; they are
  not gaps.
- Memory cards go through the BIOS in both games (see
  [`bios-status.md`](bios-status.md)).

## Validation

- **Done:** per-adapter conformance (levels 1–3) in each game that links
  it; `--check no-io` on 1,200-frame runs; native replays `title-to-gameplay`
  and `card-load` (btn) and `title-to-field` (bof4).
- **Not done:**
  - full `psxw ci` and level-4 native-vs-device replay comparison since the
    later adapter and timing changes;
  - conformance in calling contexts other than main code on the boot stack
    (scratchpad stack, inside a handler or callback, in a critical section,
    on a thread stack, partway through a safepoint quantum);
  - native and device modes share cores, so their agreement is not an
    independent oracle.

## Remaining: the rest of PsyQ 4.7

`psxw sdk scan` finds 389 of PsyQ 4.7's 3,140 library functions reach
hardware. Anything outside what btn and bof4 bind has no adapter, so a new
PsyQ 4.7 game that links it falls back to the device for that whole chip.
The prototype analysis's count of exported functions that reach hardware:

| Library | Chip | Reach hardware | Public |
|---|---|---|---|
| libspu | SPU | 107 | 93 |
| libcd | CD | 37 | 37 |
| libgpu | GPU | 36 | 36 |
| libpad | SIO0 | 28 | 11 |
| libetc | IRQ, timers | 14 | 14 |
| libmcx | SIO0 (PocketStation) | 14 | 1 |
| libapi | timers, IRQ, SIO0 | 11 | 9 |
| libpress | MDEC | 6 | 6 |
| libgun | SIO0, IRQ (light gun) | 5 | 3 |
| libsnd | SPU | 3 | 2 |
| libsio | SIO1 | 2 | 1 |
| libtap | SIO0 (multitap) | 1 | 1 |
| libcomb | SIO1 (link cable) | 1 | 0 |
| **Total** | | **265** | |

About 76 of those are adapted, plus about 45 state accessors, leaving about
190 plus their accessors.

### Work, in order

1. **Calling contexts in both harnesses** (native and `web-conform`). Run
   every existing case in each context listed under Validation, before new
   adapters land.
2. **A synthetic conformance link.** No game links the remaining functions,
   so there is no original code to compare against. Build a test program
   from PsyQ 4.7's library objects that links every function and translate
   it like a game.
3. **Device models for missing peripherals.** Each needs a model (and
   oracle vectors where the harness supports them) before its adapters can
   be more than provisional:

   | Peripheral | Used by | Today |
   |---|---|---|
   | Analog pad, DualShock | libpad | SIO0 models a digital pad only |
   | Multitap | libtap, libpad | none |
   | Light gun | libgun | none; needs GPU beam position and a host pointer |
   | SIO1 (serial, link cable) | libsio, libcomb | none, none planned |
   | CD-DA audio tracks | libcd (`CdPlay`) | the CD controller plays nothing |

4. **Adapters, chip by chip:** the rest of libspu (streaming, capture),
   libcd (`CdPlay`, the TOC), libgpu, libpad plus libtap and libgun,
   libetc and libapi, libpress, libmcx, and libsio/libcomb.
5. **Coverage gate.** `psxw sdk scan` lists every hardware-reaching function
   with its adapter and conformance status; CI fails on any function with
   neither an adapter nor a recorded exclusion.

**Done when:** every hardware-reaching PsyQ 4.7 function has an adapter or
a recorded exclusion; every adapter passes levels 1–2 on the synthetic link
in every calling context and level 3 in each game that links it; and a
PsyQ 4.7 game builds with every chip it uses native and no new adapters.
Other SDK versions are separate work.
