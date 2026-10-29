# Where the project stands: btn vs bof4

As of 2026-10-29. The function counts come from each game's `build.json`:
btn at `b37ae28`, bof4 at `1560817`.

btn isn't 100% decompiled. 817 of its functions still run translated: mostly
SDK assembly in the EXE, plus some game functions. bof4 has no decompiled
game C. Its 480 C functions are PsyQ SDK code from psyq-decomp.

```
                         btn (Harvest Moon)                     bof4 (Breath of Fire IV)
                         decomp: mostly C                       decomp: no game C
                         2,523 C · 817 lifted · 47 adapter      480 C · 14,293 lifted · 21 adapter

 ┌──────────────────┐   ┌──────────────────────────────────┐   ┌──────────────────────────────────┐
 │ GAME CODE        │   │ ████████████████████████░░░░░░░░ │   │ ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░ │
 │                  │   │ C (clang) for nearly all logic;  │   │ every game function translated   │
 │                  │   │ ~80 overlay fns + some EXE fns   │   │ from MIPS (incl. hand-written    │
 │                  │   │ still translated                 │   │ asm: decompressor, threads)      │
 ├──────────────────┤   ├──────────────────────────────────┤   ├──────────────────────────────────┤
 │ SDK LIBRARIES    │   │ libgpu libetc libpad libspu      │   │ libgpu libetc libpad libspu      │
 │ (PsyQ 4.7)       │   │ libsnd → original code ▼         │   │ libsnd libcd libds libpress      │
 │                  │   │ libcd libapi libcard → adapters  │   │ → original code ▼                │
 │                  │   │ (Rust, at the SDK's API)         │   │ libapi libcard → adapters        │
 │                  │   │ asm members translated; some C   │   │ C from psyq-decomp (480 fns),    │
 │                  │   │ from psyq-decomp                 │   │ asm members translated           │
 ├──────────────────┤   ├──────────────────────────────────┤   ├──────────────────────────────────┤
 │ BIOS             │   │ HLE BIOS in Rust                 │   │ HLE BIOS in Rust + threads       │
 │                  │   │                                  │   │ (Asyncify over 21% of fns),      │
 │                  │   │                                  │   │ kernel tables in game RAM        │
 ├──────────────────┤   ├──────────────────────────────────┤   ├──────────────────────────────────┤
 │ HARDWARE MODELS  │   │ ▓ register-level:                │   │ ▓ register-level:                │
 │ (register-level) │   │ IRQ · timers · DMA · GPU front   │   │ IRQ · timers · DMA · GPU front   │
 │                  │   │ end · SPU · SIO0 (pad)           │   │ end · SPU · SIO0 · CD controller │
 │                  │   │ CD: drive model behind adapters  │   │ · MDEC                           │
 ├──────────────────┤   ├──────────────────────────────────┤   ├──────────────────────────────────┤
 │ HOST             │   │ WebGL renderer · audio · disc · storage (same for both)              │
 ├──────────────────┤   ├──────────────────────────────────────────────────────────────────────┤
 │ CPU              │   │ none: everything compiled ahead of time to wasm (same for both)      │
 └──────────────────┘   └──────────────────────────────────────────────────────────────────────┘

 legend   █ native C (clang)   ░ recompiled from MIPS   ▼ runs on the layer below   ▓ emulated hardware
```

## Against the claim "a native port, not an emulator"

| Layer | btn | bof4 |
|---|---|---|
| CPU | ✓ not emulated | ✓ not emulated |
| Game code | mostly a native port | a static recompilation |
| SDK → hardware | mostly emulated devices; libcd at the SDK's API (adapters) | emulated devices, CD and MDEC included |
| The claim holds for… | the code, not the hardware layer | only "no CPU emulation" |

The two games differ mainly in the game-code band. Everything from the SDK
down is nearly the same machine, and that machine models hardware
registers.

## How the hardware boundary moved

D4 ruled out register-level GPU, CD and pad emulation: the SDK was to be
replaced at its API by adapters, as a native port rewrites its platform
layer. Later decisions each moved the line below the SDK, so the original
library code runs against register-level device models:

| Decision | Library | Now runs over |
|---|---|---|
| D40 | libpad | SIO0 |
| D42 | libgpu | GPU front end, DMA |
| D45 | libetc | interrupt controller, BIOS exception exit |
| D47 | libspu, libsnd | SPU |
| D58 | libcd (when not all adapters validate), libds, libpress | CD controller, MDEC |

Each step bought exactness by construction and independence from the SDK
version. Taken together, they turned the layer beneath the SDK into an
emulated device layer.
