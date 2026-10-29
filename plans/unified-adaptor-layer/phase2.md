# Unified adapter layer, phase 2: the rest of PsyQ 4.7

**Status:** not started
**Depends on:** [phase 1](phase1.md).
**Shared context:** [README.md](README.md).

## Goal

Every hardware-touching function in PsyQ 4.7 has an adapter that passes
conformance. Then any PsyQ 4.7 game runs native on every chip whose device
it uses, with no new adapters. Other SDK versions are a later plan.

## Inventory

Exported functions whose own library's code reaches hardware, from the
prototype analysis. Phase 1 covers the ones btn and bof4 use. Phase 1's
`psxw sdk scan` gives the exact remainder, state accessors included.

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

Phase 1 adapts 76 of these, plus about 45 state accessors, so phase 2 is
about 190, plus their state accessors.

## What phase 1 doesn't have

**No game links these functions,** so levels 1–3 have no original code to
run. Phase 2 needs a **synthetic conformance link**: a test program built
from PsyQ 4.7's library objects (psyq-decomp `library-input`) that links
every library function. psx-wasm translates it like a game, and it gives
the original side of every conformance case without depending on any game.

**Some chips and devices have no model,** so they have no fallback and no
oracle:

| Peripheral | Used by | Device model today | Native backend |
|---|---|---|---|
| Analog pad, DualShock | libpad | ✗ SIO0 models a digital pad only | pad source: extend |
| Multitap | libtap, libpad | ✗ | pad source: extend |
| Light gun | libgun | ✗ | needs GPU beam position and host pointer |
| SIO1 (serial, link cable) | libsio, libcomb | ✗ | none planned |
| CD-DA audio tracks | libcd (`CdPlay`) | ✗ the CD controller plays nothing | a route from audio tracks to the mixer |

For each of these, the device model comes first. An adapter without one is
provisional (README, principle 6).

## Tasks

1. [ ] **The synthetic conformance link.** Build a test program from PsyQ
   4.7's library objects and translate it with the lift. Run every phase 1
   conformance case on it too, as a cross-check against the games' links.
2. [ ] **Device models for the missing peripherals.** Analog and DualShock
   pads, the multitap, the light gun, SIO1 and CD-DA, each with oracle
   vectors where the device oracle harness supports them.
3. [ ] **Adapters, chip by chip:**
   - SPU: the rest of libspu, including streaming and capture;
   - CD: the rest of libcd, including `CdPlay` and the TOC;
   - GPU: the rest of libgpu;
   - SIO0: the rest of libpad, plus libtap and libgun;
   - IRQ and timers: the rest of libetc and libapi;
   - MDEC: the rest of libpress;
   - PocketStation: the rest of libmcx;
   - SIO1: libsio and libcomb.
4. [ ] **Coverage gate.** `psxw sdk scan` lists every hardware-reaching
   PsyQ 4.7 function, with its adapter and conformance status. CI fails on
   any function that has neither an adapter nor an explicit exclusion with
   a reason.

## Exit criteria (all automated)

1. Every function `psxw sdk scan` reports as reaching hardware in PsyQ 4.7
   has an adapter, or an exclusion with a reason.
2. Every adapter passes levels 1–2 on the synthetic link, and level 3 in
   every game that links it.
3. A PsyQ 4.7 game builds with every chip native, with no new adapters,
   for each chip whose device it uses.

## Progress log

## Findings
