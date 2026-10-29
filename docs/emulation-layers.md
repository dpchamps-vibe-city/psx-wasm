# What is emulated: the layers

As of 2026-10-29. See [status-btn-vs-bof4.md](status-btn-vs-bof4.md) for
function counts and the decisions that moved the boundary.

| Colour | Meaning |
|---|---|
| white | original code (game or SDK), compiled to wasm; no CPU is emulated |
| blue | reimplemented in Rust at a software interface (adapters, BIOS) |
| red | emulated hardware |
| green | native host backends |

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

## Today: btn

```mermaid
flowchart TB
  game["game code<br/>mostly C, some translated MIPS"]
  sdk["SDK code, original<br/>libgpu · libetc · libpad · libspu · libsnd · libgs · libc"]
  adapt["adapters<br/>libcd · libapi · libcard · libmcx"]
  bios["BIOS (Rust)"]
  gte["GTE"]

  subgraph emuLayer ["EMULATED LAYER"]
    regs["I/O registers 0x1F801000+<br/>checked on every load and store"]
    irq["IRQ controller"]
    tim["timers"]
    dma["DMA"]
    gpu["GPU<br/>front end + rasterizer"]
    spu["SPU"]
    sio["SIO0 (pad)"]
  end

  drive["CD drive model<br/>seek and read timing"]

  subgraph hostRow ["HOST"]
    gl["WebGL"]
    audio["audio out"]
    disc["disc image"]
    pad["pad input"]
    cards["card files"]
  end

  game --> sdk
  game --> adapt
  game --> gte
  sdk --> bios
  sdk --> regs
  adapt --> drive
  adapt --> bios
  regs --> irq & tim & dma & gpu & spu & sio
  dma --> gpu
  dma --> spu
  gpu --> gl
  spu --> audio
  sio --> pad
  drive --> disc
  bios --> cards
  irq -. "interrupts" .-> bios
  bios -. "handler chains" .-> sdk

  classDef orig fill:#ffffff,stroke:#333,color:#000
  classDef rust fill:#d6e4f5,stroke:#36c,color:#000
  classDef emu fill:#f8d0d0,stroke:#b33,color:#000
  classDef native fill:#d8f0d8,stroke:#383,color:#000
  class game,sdk orig
  class adapt,bios rust
  class gte,regs,irq,tim,dma,gpu,spu,sio,drive emu
  class gl,audio,disc,pad,cards native
  style emuLayer fill:#fdecec,stroke:#b33,stroke-dasharray: 5 5
```

## Today: bof4

No CD adapters: libcd, libds and libpress run as original code over the
emulated CD controller and MDEC.

```mermaid
flowchart TB
  game["game code<br/>all translated MIPS"]
  sdk["SDK code, original<br/>libgpu · libetc · libpad · libspu · libsnd<br/>libcd · libds · libpress · libgs · libc"]
  adapt["adapters<br/>libapi · libcard · libmcx"]
  bios["BIOS (Rust)<br/>+ threads"]
  gte["GTE"]

  subgraph emuLayer ["EMULATED LAYER"]
    regs["I/O registers 0x1F801000+<br/>checked on every load and store"]
    irq["IRQ controller"]
    tim["timers"]
    dma["DMA"]
    gpu["GPU<br/>front end + rasterizer"]
    spu["SPU"]
    sio["SIO0 (pad)"]
    cdc["CD controller<br/>+ XA audio"]
    mdec["MDEC"]
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
  game --> gte
  sdk --> bios
  sdk --> regs
  adapt --> bios
  regs --> irq & tim & dma & gpu & spu & sio & cdc & mdec
  dma --> gpu
  dma --> spu
  dma --> cdc
  dma --> mdec
  cdc --> spu
  gpu --> gl
  spu --> audio
  sio --> pad
  cdc --> disc
  bios --> cards
  irq -. "interrupts" .-> bios
  bios -. "handler chains" .-> sdk

  classDef orig fill:#ffffff,stroke:#333,color:#000
  classDef rust fill:#d6e4f5,stroke:#36c,color:#000
  classDef emu fill:#f8d0d0,stroke:#b33,color:#000
  classDef native fill:#d8f0d8,stroke:#383,color:#000
  class game,sdk orig
  class adapt,bios rust
  class gte,regs,irq,tim,dma,gpu,spu,sio,cdc,mdec emu
  class gl,audio,disc,pad,cards native
  style emuLayer fill:#fdecec,stroke:#b33,stroke-dasharray: 5 5
```

## An adapter build

Every SDK function that touches hardware gets an adapter; the rest of the
SDK still runs as original code. Nothing reaches the I/O registers, so a
build with no I/O bus traps on any access: the claim is checkable per
game. The cost is a validated adapter set per SDK version.

```mermaid
flowchart TB
  game["game code"]
  sdk["SDK code, original<br/>only what touches no hardware:<br/>libgs · libgte · libc · libsnd's sequencer"]
  adapt["adapters<br/>libgpu · libetc · libpad · libspu · libcd · libds<br/>libpress · libapi · libcard · libmcx"]
  bios["BIOS (Rust)"]
  gte["GTE<br/>still emulated"]

  subgraph back ["NATIVE BACKENDS"]
    render["renderer<br/>still decodes PS1 packets"]
    mixer["mixer<br/>voices, reverb"]
    reader["disc reader"]
    video["video decoder"]
    input["pad state"]
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
  adapt --> render & mixer & reader & video & input
  adapt --> bios
  render --> gl
  mixer --> audio
  reader --> disc
  input --> pad
  bios --> cards
  adapt -. "callbacks: vblank, completion" .-> game

  classDef orig fill:#ffffff,stroke:#333,color:#000
  classDef rust fill:#d6e4f5,stroke:#36c,color:#000
  classDef emu fill:#f8d0d0,stroke:#b33,color:#000
  classDef native fill:#d8f0d8,stroke:#383,color:#000
  class game,sdk orig
  class adapt,bios rust
  class gte emu
  class render,mixer,reader,video,input,gl,audio,disc,pad,cards native
```
