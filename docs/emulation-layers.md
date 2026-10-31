# What is emulated: the layers

The build modes in the [unified adaptor layer plan](../plans/unified-adaptor-layer/README.md):
prefer native software interfaces, then fall back to hardware registers
one chip at a time. These are architectural configurations, not snapshots
of btn or bof4. See [adaptor-layer-status.md](adaptor-layer-status.md) for
current game coverage, function counts, and remaining work.

| Colour | Meaning |
|---|---|
| white | original code (game or SDK), compiled to wasm; no CPU is emulated |
| blue | reimplemented in Rust at a software interface (adaptors, BIOS) |
| red | reproduced hardware semantics (register-level devices, GTE) |
| green | native backends and host services |

Solid arrows are calls and accesses. Dotted arrows are interrupts and
callbacks, which go up.

## The PS1

```mermaid
flowchart TB
  game["game code"]
  sdk["SDK code<br/>(linked into the game; contains the drivers)"]
  bios["BIOS ROM"]
  gte["GTE (COP2)"]
  regs["hardware registers<br/>0x1F801000+"]
  gpu["GPU"]
  spu["SPU"]
  cd["CD controller"]
  more["MDEC · SIO0 · timers · DMA · IRQ"]

  game --> sdk
  game --> gte
  sdk --> gte
  sdk --> bios
  sdk --> regs
  regs --> gpu & spu & cd & more
  more -. "interrupts" .-> bios
  bios -. "handler chains" .-> sdk
```

## What is an adaptor?

An adaptor is a Rust replacement for an SDK function at a validated
software boundary. Instead of running the original driver through hardware
registers, it calls the native core directly: submit draw packets, mix
voices, read sectors, decode video, or sample pad state. It preserves what
the code above that boundary observes: return values, buffers, shared
library state, callbacks and timing at sync points. The same adaptor serves
every game that uses that function; it is not a per-game shortcut.

Only the boundary is replaced. Code above it stays original: libgs builds
packets, libsnd sequences music, libds drives libcd, and helpers can remain
where they do not reach past the adaptors. Adaptors for one chip share one
state model, including calls from different libraries.

There are two kinds:
- **Hardware adaptors** replace SDK code that drives a chip. Missing or
  unvalidated coverage makes that whole chip fall back to its device model.
- **BIOS-internals adaptors** replace code that patches or reads Sony BIOS
  internals (parts of libapi, libcard and libmcx). These are mandatory:
  the BIOS is Rust, so there is no Sony ROM to fall back to.

**The GTE is fixed-point geometry, rebuilt in Rust.** COP2 instructions and
`gte_*` macros call `psx-gte`, a maths library retaining the PS1's register,
rounding, saturation and flag behaviour. It stays red because it reproduces
hardware semantics, but runs as compiled maths alongside the game—not as
an emulated CPU. This is unchanged by adaptor coverage.

## Full adaptor build

Every chip the game uses has complete, validated adaptor coverage. No
original driver reaches the I/O registers. The renderer still understands
PS1 packets, the mixer still implements voices and reverb, and the drive
still has seek/read timing; bypassing register protocols does not remove
those behaviours. Adaptors and device models use the same underlying cores.

```mermaid
flowchart TB
  game["game code"]
  sdk["SDK code, original<br/>libgs · libgte · libc · libds · libsnd's sequencer<br/>and helpers above the adaptor boundary"]
  adapt["adaptors<br/>libgpu · libetc · libpad · libspu · libcd · libpress<br/>+ BIOS-internals and shared-state adaptors"]
  bios["BIOS (Rust)"]
  gte["GTE<br/>fixed-point geometry, rebuilt in Rust"]

  subgraph back ["NATIVE BACKENDS"]
    render["renderer<br/>still decodes PS1 packets"]
    mixer["mixer<br/>voices, reverb"]
    reader["disc reader<br/>seek and read timing"]
    video["video decoder"]
    input["pad state"]
    clock["virtual clock<br/>vblank, timers, completions"]
  end

  subgraph hostRow ["HOST"]
    gl["WebGL"]
    audio["audio out"]
    disc["disc image"]
    pad["pad input"]
    cards["card files"]
  end

  game --> sdk
  game --> adapt
  sdk --> adapt
  game --> gte
  sdk --> gte
  sdk --> bios
  adapt --> render & mixer & reader & video & input & clock
  adapt --> bios
  reader -- "XA audio" --> mixer
  render --> gl
  mixer --> audio
  reader --> disc
  input --> pad
  bios --> cards
  adapt -. "callbacks: vblank, completion" .-> game
  adapt -. "library callbacks" .-> sdk

  classDef orig fill:#ffffff,stroke:#333,color:#000
  classDef rust fill:#d6e4f5,stroke:#36c,color:#000
  classDef emu fill:#f8d0d0,stroke:#b33,color:#000
  classDef native fill:#d8f0d8,stroke:#383,color:#000
  class game,sdk orig
  class adapt,bios rust
  class gte emu
  class render,mixer,reader,video,input,clock,gl,audio,disc,pad,cards native
```

A full build can assert `--check no-io`: any guest register access is a
coverage bug, not an invitation to mix drivers. Phase 1 targets this mode
for btn and bof4; phase 2 extends coverage to the rest of PsyQ 4.7.

## Partial adaptor build: graceful degradation

If anything reaches a chip outside its validated adaptor set, **the whole
chip falls back**, not just that function or library. Its original SDK
drivers run over the register-level device. Other chips keep their native
paths; a chip never has an adaptor driver and an original driver competing
for its state.

Here, CD and MDEC have fallen back. GPU, SPU, pads and the SDK's
interrupt/timer layer remain adapted. The fallback devices retain the DMA
and IRQ support they need; adapting the SDK's dispatcher does not erase
shared controller state. This is an illustrative configuration, not a
particular game's current build.

```mermaid
flowchart TB
  game["game code"]
  sdk["SDK code, original<br/>libgs · libgte · libc · libds · libsnd's sequencer"]
  drivers["SDK drivers, original<br/>libcd · libpress"]
  adapt["adaptors<br/>GPU · SPU · pad · interrupts/timers<br/>+ mandatory BIOS-internals adaptors"]
  bios["BIOS (Rust)"]
  gte["GTE<br/>fixed-point geometry, rebuilt in Rust"]

  subgraph emuLayer ["DEVICE FALLBACK + SHARED SUPPORT"]
    regs["I/O registers 0x1F801000+<br/>accessed by the original drivers"]
    dma["DMA"]
    irq["IRQ controller"]
    cdc["CD controller<br/>+ XA audio"]
    mdec["MDEC"]
  end

  subgraph back ["NATIVE BACKENDS"]
    render["renderer<br/>still decodes PS1 packets"]
    mixer["mixer<br/>voices, reverb"]
    input["pad state"]
    clock["virtual clock<br/>vblank, timers, completions"]
  end

  subgraph hostRow ["HOST"]
    gl["WebGL"]
    audio["audio out"]
    disc["disc image"]
    pad["pad input"]
    cards["card files"]
  end

  game --> sdk
  game --> drivers
  game --> adapt
  game --> gte
  sdk --> drivers
  sdk --> adapt
  sdk --> gte
  sdk --> bios
  drivers --> regs
  drivers -- "callback setup" --> adapt
  adapt --> bios
  adapt --> render & mixer & input & clock
  regs --> dma & irq & cdc & mdec
  dma --> cdc & mdec
  dma --> irq
  cdc --> irq
  cdc -- "XA audio" --> mixer
  cdc --> disc
  render --> gl
  mixer --> audio
  input --> pad
  bios --> cards
  irq -. "interrupts" .-> bios
  bios -. "interrupt dispatch" .-> adapt
  adapt -. "completion callbacks" .-> drivers
  adapt -. "library callbacks" .-> sdk
  adapt -. "callbacks: vblank, completion" .-> game

  classDef orig fill:#ffffff,stroke:#333,color:#000
  classDef rust fill:#d6e4f5,stroke:#36c,color:#000
  classDef emu fill:#f8d0d0,stroke:#b33,color:#000
  classDef native fill:#d8f0d8,stroke:#383,color:#000
  class game,sdk,drivers orig
  class adapt,bios rust
  class gte,regs,dma,irq,cdc,mdec emu
  class render,mixer,input,clock,gl,audio,disc,pad,cards native
  style emuLayer fill:#fdecec,stroke:#b33,stroke-dasharray: 5 5
```

As coverage shrinks, more chips move behind the register bus. At the far
end, all hardware-facing SDK code runs original over device models—but
the game and SDK are still compiled, never CPU-interpreted. The degradation
is in how much hardware must be modelled, not a deliberate loss of game
behaviour or output quality. BIOS-internals adaptors and the Rust GTE stay
in every mode.

The build reports each chip's mode and fallback reason in `build.json`;
`game.toml` can require chips to stay native, making unexpected degradation
a build error. `--device <chips|all>` forces fallback for conformance and
replays; run reports count register accesses per chip.
