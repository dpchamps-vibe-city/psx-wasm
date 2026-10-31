# Adaptor layer status

As of 2026-10-30, reviewed at project revision `c0c34e7`.

## Current game builds

**Both games under test, btn and bof4, now use native adapters for every chip group in their normal
builds.** Neither build selects register-level device fallback. Both local
`build.json` reports say:

```text
emulated: none; 0 of 6 chips
```

This replaces the earlier arrangement in which most original SDK drivers ran
against emulated device registers. The main difference between the games is
now their game code: Harvest Moon: Back to Nature (`btn`) is mostly compiled
from decompiled C; Breath of Fire IV (`bof4`) still uses translated MIPS for
all game logic.

## How the code runs

All game and SDK code is compiled ahead of time to WebAssembly. Neither game
uses a CPU interpreter or a CPU emulator.

| Function implementation | btn | bof4 |
| --- | ---: | ---: |
| Compiled from C | 2,574 | 480 |
| Translated from MIPS | 727 | 14,250 |
| Replaced by a Rust adapter | 86 | 92 |
| Game source revision | `c77a66f` | `57f7da1` |

These counts include both game and SDK functions. They come from the local
`out/btn/build.json` and `out/bof4/build.json` reports for the source revisions
pinned in [`inputs.lock`](../inputs.lock), not from a fresh rebuild for this
update.

btn is not fully decompiled: some game functions and SDK assembly still need
translation. bof4's 480 C functions come from the PsyQ SDK sources, not from
decompiled game logic.

## What the adapters replace

An adapter is a Rust replacement for an SDK function that controls hardware.
It calls the renderer, mixer, disc reader, or another shared implementation
directly instead of running the original driver through hardware-register
accesses. The same adapter implementation serves both games.

The build chooses this path for a whole chip group, not for isolated calls.
The current selections are:

| Chip group | Replaced SDK code | btn | bof4 |
| --- | --- | --- | --- |
| Graphics (`gpu`) | Hardware-facing libgpu functions | Native | Native |
| Sound (`spu`) | Hardware-facing libspu functions and libsnd initialization | Native | Native |
| CD drive (`cd`) | libcd's low-level drive functions and streaming interrupt handler | Native | Native |
| Controllers (`sio0`) | libpad's controller driver | Native | Native |
| Interrupts and timers (`irq`) | libetc's interrupt handling and libapi's counter functions | Native | Native |
| Video decoder (`mdec`) | libpress's decoder functions | Not linked | Native |
| Card-port driver (`memcard`) | libmcx's card-port functions | Native | Not linked |

The `memcard` row describes libmcx, the PocketStation library. It does not mean
bof4 has no memory-card support: BIOS card services remain available in both
games.

Only the hardware-facing functions and the state accessors that need to change
are replaced. Other SDK code still runs as compiled C or translated MIPS. For
example, libds and libcd's public functions use the adapted CD drive functions;
libsnd's music sequencer uses the adapted sound functions. Adapters preserve the
library state, callbacks, and timing that this remaining code expects.

Both game configurations require all six of their reported chip groups to stay
native. An unexpected fallback is a build error. The requirements are in
[`games/btn/game.toml`](../games/btn/game.toml) and
[`games/bof4/game.toml`](../games/bof4/game.toml).

## What still reproduces PlayStation behaviour

“Native” describes how the game reaches a subsystem. It does not mean that
PlayStation behaviour has been replaced with unrelated host behaviour:

- **Graphics and sound:** the renderer still understands PlayStation draw
  commands, and the mixer still implements the console's voices and reverb.
- **Disc and video:** reads retain drive timing, and the video decoder still
  decodes the game's original movie data.
- **Interrupts, timers, and transfers:** adapters use the shared interrupt,
  timer, and DMA controller state. DMA is the mechanism for moving data between
  memory and devices. Some adapters still use controller register semantics
  internally; this is distinct from original guest code accessing I/O registers.
- **Geometry:** the GTE, the PlayStation's geometry coprocessor, remains a Rust
  implementation of its fixed-point arithmetic, registers, and flags. It is
  not included in the six chip groups reported above.
- **BIOS and threads:** BIOS services are implemented in Rust. bof4's thread
  switching uses Asyncify to suspend and restore compiled Wasm calls, not an
  emulated CPU stack.

The register-level device implementations have not been deleted. They remain
available for other games, unsupported SDK variants, and comparison tests.
`--device <chip>` or `--device all` explicitly selects them. Adapters and device
fallbacks also share underlying implementations, so a bug in a shared renderer,
mixer, or decoder can affect both modes.

The accurate description is therefore: **compiled game code with native SDK
adapters, no selected device fallback in either normal build, and retained
PlayStation behaviour where the game depends on it.** This does not make bof4
a source-level C port or remove the remaining MIPS translation from btn.

## What has been checked

The [adapter implementation record](../plans/unified-adaptor-layer/phase1.md)
documents these results:

- Both games report all chip groups native. The recorded 1,200-frame runs
  passed `--check no-io`, which checks that guest code made no I/O-register
  accesses.
- Each chip's adapter tests passed against the original SDK code running over
  the device fallback in the games that link it. Some suites were rerun after
  later timing changes; not all were.
- Recorded native replays passed for btn's `title-to-gameplay` and `card-load`,
  and bof4's `title-to-field`.
- Full `psxw ci` runs and complete native-versus-device replay comparisons were
  not rerun after all the adapter and timing changes. These results are not a
  claim of complete validation at the current revision.

A zero-I/O result covers the paths reached by that run, not every possible game
state. Agreement between native and device modes is also not an independent
hardware check, because the two modes share implementations.

To rebuild and check a short run with the local game inputs:

```sh
psxw build btn
psxw build bof4
psxw run btn --frames 1200 --check no-io
psxw run bof4 --frames 1200 --check no-io
```

## Remaining work

The two game builds do not establish complete SDK coverage. They use a subset
of PsyQ 4.7, and another game can need functions or peripherals that neither
uses. The [next phase](../plans/unified-adaptor-layer/phase2.md) covers the rest
of PsyQ 4.7; other SDK versions are separate work.

### Device fallback for other games

The following register-level models remain available when a game's SDK code
cannot use the adapters. In that case, the original driver runs over the model;
the game and driver are still compiled to Wasm, not CPU-emulated.

| Device model | Coverage still to extend |
| --- | --- |
| GPU and its transfers | libgpu functions and library variants outside the tested set |
| SPU and its transfers | Remaining libspu functions, including streaming and capture |
| CD controller | Remaining libcd functions and library variants |
| SIO0 controller and card ports | Additional libpad and libmcx driver paths |
| MDEC video decoder | libpress variants beyond the validated objects |
| Interrupt controller, timers, and DMA | Remaining libetc/libapi functions and the support needed by fallback drivers |

An uncovered function can make its whole chip group fall back, while other
chips keep their native adapters. These models are not scheduled for removal:
they also provide the comparison path for adapter tests. Adding support for a
new SDK version requires validating its library code; having a device model
alone does not establish that a game using it is supported.

### Peripherals that are not yet supported

Some gaps are not emulated fallbacks. The current runtime has no complete model
or native path for:

- Analog and DualShock controllers; the pad model is digital only.
- Multitaps and light guns.
- SIO1 serial and link-cable communication.
- CD-DA audio-track playback, as distinct from the supported XA audio in disc
  sectors.

These need device and host support as well as adapters. The plan calls for
reference tests before treating their behaviour as validated.

### Tests and broader SDK coverage

The remaining SDK functions need a test program that links them even when
neither game does. That will let the same adapter tests compare their behaviour
with the original library code. Tests also need coverage of calls from interrupt
handlers, critical sections, thread stacks, and scratchpad memory—not just
ordinary calls from the main game loop.

The SDK scan should then account for every hardware-facing function and relevant
state accessor: either a tested adapter or an explicit exclusion with a reason.
The full CI and native-versus-device replay checks noted above remain part of
this work.

The GTE's fixed-point behaviour, BIOS services, graphics and audio formats, and
shared timing and DMA rules are deliberate compatibility requirements. They
remain necessary even with complete SDK adapter coverage.

For the full and partial adapter configurations, see
[`emulation-layers.md`](emulation-layers.md).
