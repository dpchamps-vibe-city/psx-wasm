# psx-wasm execution plan

This directory is the execution plan for building psx-wasm end to end. It's
written to be the source for an autonomous `/goal`. Every plan file is
self-contained, so an agent with no memory of earlier sessions can pick up
where the last one stopped.

| File | What it holds |
|---|---|
| [`../docs/design.md`](../docs/design.md) | The source of truth for *what* we build and *why*, ending in the decision log (D1–D58). Plans say *how* and *in what order*. |
| [`conventions.md`](conventions.md) | Repo layout, commands, file formats, data hygiene, commit rules |
| [`unified-adaptor-layer/`](unified-adaptor-layer/) | **The active plan:** native adapters per chip, with graceful degradation to the device models. A README for shared context, then phase 1 (btn and bof4) and phase 2 (the rest of PsyQ 4.7). |
| [`open-items.md`](open-items.md) | Every unfinished task, blocked item, human-needed input and erratum, in one place |
| [`completed/`](completed/) | Everything implemented: milestones M0–M13 (M9 never started) and the btn bug follow-ups, kept as history |

## What psx-wasm is, in one screen

psx-wasm takes a PlayStation decompilation (matching C where there is any;
translated MIPS for the rest) and produces `game.wasm`, which runs in a
browser Web Worker or under wasmtime.

- **The CPU is gone.** Game and SDK C compile to wasm with clang. Code with
  no C is translated ahead of time from the original MIPS into generated C.
  Nothing interprets MIPS at run time.
- **The memory map is kept.** Every symbol sits at its original address in
  a 16 MiB window of linear memory. Pointers keep their PSX values; only the
  effective address is masked. Function pointers are MIPS addresses.
- **Below the SDK, hardware is modelled at register level today.** Device
  libraries (libgpu, libetc, libpad, libspu, libsnd, and libcd where its
  adapters don't validate) run as original code against register-level
  models of the interrupt controller, timers, DMA, GPU, SPU, SIO0, CD
  controller and MDEC. Rust adapters replace SDK functions only where a
  library's policy says so (`boundary/psyq.toml`). The BIOS is a Rust
  reimplementation. The unified adapter layer plan moves each chip back to
  native adapters wherever they validate.
- **Time is virtual and deterministic.** It moves only inside runtime calls
  and at safepoints. Callbacks are delivered at safepoints.
- **Every component needs an automated oracle we didn't write.** The
  oracles are the original code (decompiled C, original libraries),
  results that are exact by construction, and device targets. A device
  target is a dockerized headless emulator now, a real PS1 later.

Two games: Harvest Moon: Back to Nature (`games/btn`, mostly decompiled C)
and Breath of Fire IV (`games/bof4`, no game C), both built with PsyQ 4.7.
Nothing in code may be specific to either.

## How to execute this plan

1. **Read** this README, `conventions.md`, then the design sections and
   decisions the plan cites.
2. **Pick** the active plan's first phase whose status isn't `done`, and
   its first unticked task. Items in `open-items.md` are picked up only
   when a plan or the human asks.
3. **Work the tasks in order.** Keep the plan file current, because it's
   the memory across sessions:
   - tick tasks as they complete;
   - append to *Progress log*: date, what changed, the commit;
   - append to *Findings*: anything learned that changes later work.
4. **A phase is done** only when every command in its *Exit criteria*
   passes from a clean checkout of the pinned inputs, the docs reflect
   reality, and the work is committed. Then move what's finished to
   `completed/` and anything left to `open-items.md`.
5. **Never weaken an oracle to make it pass.** That includes loosening a
   comparison, excluding cases without a recorded reason, or marking a
   provisional verdict verified. If an oracle can't pass, record why in
   Findings, mark the task `blocked`, and continue with an unblocked one
   or stop and ask the human.
6. **Design changes go to `docs/design.md`:** the body states what is
   true, and the decision log records why. Ask the human before any
   decision that changes the design.
7. **Commit** at coherent points with descriptive messages. Never add
   `Co-Authored-By` or any Claude/Claude Code attribution.

## Plans

| Plan | Status | Depends on |
|---|---|---|
| [Unified adapter layer, phase 1](unified-adaptor-layer/phase1.md): every adapter btn and bof4 need; both games native except the GTE | not started | M13 |
| [Unified adapter layer, phase 2](unified-adaptor-layer/phase2.md): the rest of PsyQ 4.7 | not started | phase 1 |

## Completed

| # | Milestone | Gating oracle | Open items left |
|---|---|---|---|
| [M0](completed/M00-census.md) | Census | GCC 2.95 vs clang layout probe; coverage; references | — |
| [M1](completed/M01-translator-and-linker-core.md) | Translator and linker core | Translated == decompiled C, every function that has both | — |
| [M2](completed/M02-whole-game-link-and-boot.md) | Whole-game link and headless boot | Boot RAM == EXE; CD sectors verify; overlays dispatch; main loop; determinism | — |
| [M3](completed/M03-oracle-harness.md) | Oracle harness (Docker) | Exact-by-construction vectors reproduce through the emulator | whole-game RAM equality waits on CD timing (D48) |
| [M4](completed/M04-pixels.md) | Pixels | GTE/GPU vectors byte-exact vs goldens; adapter conformance; RAM by symbol | task 11 (D48) |
| [M5](completed/M05-browser.md) | Browser host | Cross-host determinism (browser == native hashes) | — |
| [M6](completed/M06-sound.md) | Sound | SPU/XA vectors vs goldens; libspu conformance | task 10 (D48) |
| [M7](completed/M07-saves-and-play-session.md) | Saves and full play session | Card checksums; game accepts its saves; RAM by symbol over a day | the human's recordings; D48 |
| [M8](completed/M08-sdk-generality.md) | SDK generality | Per-object adapter conformance; psyq-decomp C == translated | the .LIB converter; psyq-decomp C |
| [M10](completed/M10-hardware-oracle.md) | Hardware oracle | Vector archive replayed on hardware | everything that needs a rig |
| [M11](completed/M11-modkit.md) | Modkit (compiled mods, deprecated for M12, D56) | Example mods' effects by replay; static and dynamic backends agree | — |
| [M12](completed/M12-script-mods.md) | Script mods | Script twins == compiled mods; a pass-through on every hookable function == plain build | — |
| [M13](completed/M13-bof4-static-recompilation.md) | Breath of Fire IV: a game with no decompiled C | A headless replay to the first field scene: determinism, no traps, reached-state checks | FMV differences; the harness push |
| [Bugs](completed/bug-follows.md) | Bugs found playing btn in the browser | — | see open-items |

M9 (a second decompiled game) never started; it is an open item.

**Human-needed inputs** are listed in [`open-items.md`](open-items.md).

## Repository map

```
inputs.lock              pinned repos, toolchains, Docker images, disc hashes
psxw.local.toml          (gitignored) machine-local paths: disc images, overrides
games/<id>/game.toml     everything game-specific
boundary/                SDK library policies: adapter, device, adapter-or-device, guest
docs/design.md           architecture
plans/                   this plan
crates/
  psxw-abi               guest/runtime/host contract, memory layout, ABI version
  psxw-inputs            lockfile, snapshots, splat, symbols, address map, originals
  psxw-cc                C tier: clang driver, prelude, normalizer, layout probe, prototype facts
  psxw-census            M0 census
  psxw-lift              translator: MIPS → generated C                         (M1)
  psxw-link              object reader, linker, guest passes                    (M1–M2)
  psxw-difftest          function-level differential harness                    (M1)
  psxw-oracle            vectors, goldens, provenance, compare, Docker targets  (M3)
  psx-gte                GTE                                                    (M4)
  psx-devices            gpu front end, spu, cd, sio, clock                     (M2→)
  psx-bios               BIOS A0/B0/C0 + syscall                                (M2)
  sdk-psyq               PsyQ adapters                                          (M2→)
  psx-runtime            boot, scheduler, guest memory, host interface; builds runtime.wasm (M2)
  psx-render-gl          WebGL2/GLES backend over glow                          (M4)
  psxw-host-native       wasmtime host: headless runs, replays, tests           (M2)
  psxw-host-web          browser host (wasm-bindgen) + web/ shell               (M5)
  psxw-cli               `psxw` driver
oracle/
  docker/                Dockerfiles (Beetle PSX + OpenBIOS + host; PSn00bSDK)  (M3)
  emulator-host/         headless libretro frontend (C), built in Docker        (M3)
  runner/                PS1 runner program (C, PSn00bSDK)                      (M3)
web/                     TypeScript shell: worker bootstrap, AudioWorklet, UI   (M5)
```

The legacy `psx-runtime` stub is split into `psx-runtime`, `psx-bios`,
`psx-devices` and `sdk-psyq` in M2.

## Glossary

| Term | Meaning |
|---|---|
| **Guest** | The game plus SDK code compiled or translated into `game.wasm`. |
| **Window** | Linear memory `[0x0100_0000, 0x0200_0000)`, holding PSX physical addresses `& 0x00FF_FFFF`. |
| **Tier** | How a function is implemented: `c`, `lift` (translated) or `adapter`. |
| **Adapter** | A Rust function replacing an SDK function at its API. *Hardware* adapters drive a chip's native backend and can fall back to its device; *BIOS-internals* adapters replace code that patches the Sony BIOS and are mandatory. |
| **Device** | A register-level model of a chip that original SDK code drives: interrupt controller, timers, DMA, GPU, SPU, SIO0, CD controller, MDEC; plus the GTE and the clock. |
| **Vector** | One recorded device input (e.g. a draw batch plus the VRAM state before it). |
| **Golden** | A device target's result for a vector, with its provenance. |
| **Provenance** | Where a golden came from: `hardware` or `emulator:<name>@<commit>`, plus the BIOS hash and runner version. |
| **Verified** | Checked against a true oracle: original code, exact by construction, or hardware. |
| **Provisional** | Checked against an emulator golden. |
| **Unresolved** | No golden yet, or the emulator is a known-weak oracle for it. |
| **Replay** | A per-frame input log; with the same disc and build it determines a run exactly. |
