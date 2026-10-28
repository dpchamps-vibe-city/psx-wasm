# psx-wasm execution plan

This directory is the execution plan for building psx-wasm end to end. It's
written to be the source for an autonomous `/goal` ("implement the whole
project"). Every milestone file is self-contained, so an agent with no
memory of earlier sessions can pick up where the last one stopped.

- **Architecture:** [`docs/design.md`](../docs/design.md) is the source of
  truth for *what* we build and *why*. These plans say *how* and *in what
  order*.
- **Conventions:** [`conventions.md`](conventions.md) covers repo layout,
  commands, file formats, data hygiene and commit rules.
- **Decisions:** [`decisions.md`](decisions.md) is the log of settled
  decisions and their reasons. Don't relitigate them; propose a change in
  the milestone's Findings instead.
- **Done:** finished milestones live in [`completed/`](completed/) (M0–M6;
  M4 and M6 each keep one task blocked on CD timing, D48).
- **What's left:** [`follow-up-tasks.md`](follow-up-tasks.md) gathers every
  open task, what each needs, and the gaps found along the way.

## What psx-wasm is, in one screen

psx-wasm takes a matching PlayStation decompilation (plus, optionally, a
decompiled PsyQ SDK) and produces `game.wasm`, which runs natively in a
browser Web Worker or under wasmtime.

- **The CPU is gone.** Game and SDK C compile to wasm with clang. Code with
  no C is translated ahead of time from the original MIPS into generated C.
  Nothing interprets MIPS at run time.
- **The memory map is kept.** Every symbol sits at its original address in
  a 16 MiB window of linear memory. Pointers keep their PSX values; only the
  effective address is masked. Function pointers are MIPS addresses.
- **Hardware is replaced at the SDK's device edge.** A function gets a thin
  Rust *adapter* only if it touches hardware registers or BIOS internals.
  Everything else runs as guest code. The Rust runtime is the PlayStation's
  OS and devices as a library: a BIOS dispatcher, plus devices (GPU front
  end, SPU, CD, SIO, clock, GTE) that speak the hardware's own vocabulary.
- **Time is virtual and deterministic.** It moves only inside runtime calls
  and at safepoints. Callbacks are delivered at safepoints.
- **Every component needs an automated oracle we didn't write.** The
  oracles are the original code (decompiled C, original libraries),
  results that are exact by construction, and device targets. A device
  target is a dockerized headless emulator now, a real PS1 later.

The first target game is Harvest Moon: Back to Nature (`games/btn`), built
with PsyQ 4.7. Nothing in code may be specific to it.

## How to execute this plan

1. **Read** this README, `conventions.md` and `decisions.md`, then the
   design sections the milestone cites.
2. **Pick** the first milestone in the index whose status isn't `done` and
   whose dependencies are `done`. Milestones marked *parallel* may be
   interleaved.
3. **Work its Tasks in order.** Keep the milestone file current, because
   it's the memory across sessions:
   - tick tasks as they complete;
   - append to *Progress log*: date, what changed, the commit;
   - append to *Findings*: anything learned that changes later work.
4. **A milestone is done** only when every command in its *Exit criteria*
   passes from a clean checkout of the pinned inputs, the docs reflect
   reality, and the work is committed. Then set its status to `done` here
   and in the file.
5. **Never weaken an oracle to make it pass.** That includes loosening a
   comparison, excluding cases without a recorded reason, or marking a
   provisional verdict verified. If an oracle can't pass:
   - record why in Findings;
   - mark the milestone `blocked`;
   - continue with an unblocked parallel milestone, or stop and ask the
     human.
6. **Ask the human** for anything in *Human-needed inputs* below, and
   whenever a decision would change `docs/design.md` or `decisions.md`.
7. **Commit** at coherent points with descriptive messages. Never add
   `Co-Authored-By` or any Claude/Claude Code attribution.

## Milestone index

| # | Milestone | Status | Depends on | Gating oracle |
|---|---|---|---|---|
| [M0](completed/M00-census.md) | Census | **done** | — | GCC 2.95 vs clang layout probe; coverage; references |
| [M1](completed/M01-translator-and-linker-core.md) | Translator and linker core | **done** | M0 | Translated == decompiled C, every function that has both |
| [M2](completed/M02-whole-game-link-and-boot.md) | Whole-game link and headless boot | **done** | M1 | Boot RAM == EXE; CD sectors verify; overlays dispatch; main loop; determinism |
| [M3](completed/M03-oracle-harness.md) | Oracle harness (Docker) | **done** (whole-game RAM equality is a diagnostic while CD timing is unresolved, D48) | M0 (*parallel* with M1–M2) | Exact-by-construction vectors reproduce through the emulator |
| [M4](completed/M04-pixels.md) | Pixels | **done** but task 11 (blocked on CD timing, D48) | M2, M3 | GTE/GPU vectors byte-exact vs goldens; adapter conformance; RAM by symbol |
| [M5](completed/M05-browser.md) | Browser host | **done** (cross-host determinism over `title-to-gameplay`; pacing reported) | M4 | Cross-host determinism (browser == native hashes) |
| [M6](completed/M06-sound.md) | Sound | **done** but task 10 (blocked on CD timing, D48) | M2, M3 (*parallel* with M4–M5) | SPU/XA vectors vs goldens; libspu conformance |
| [M7](M07-saves-and-play-session.md) | Saves and full play session | **in progress** (memory cards: device, HLE BIOS, libmcrd as guest code; native and OPFS storage) | M4, M6 | Card checksums; game accepts its saves; RAM by symbol over a day |
| [M8](M08-sdk-generality.md) | SDK generality | **in progress** (identity, boundary, layouts, conformance; the .LIB converter and psyq-decomp C remain) | M2 (*parallel* with M4–M7) | Per-object adapter conformance; psyq-decomp C == translated |
| [M9](M09-second-game.md) | Second game | not started | M7, M8 | All gates with only a manifest |
| [M10](M10-hardware-oracle.md) | Hardware oracle | **in progress** (batch export/import, disagreement report; capture waits for a rig) | M3; a rig | Vector archive replayed on hardware |
| [M11](M11-modkit.md) | Modkit | **done** (browser runs of acceptance 5 and 7 waived); compiled mods deprecated for M12's script mods (D56) | M2 (phase 1), M5 (phase 2) | Example mods' effects by replay; empty mod set == plain build; static and dynamic backends agree |
| [M12](M12-script-mods.md) | Script mods | **done** | M11 | Script twins == compiled mods bit for bit in both browsers; a script pass-through on every hookable function == plain build |

```
M0 ─► M1 ─► M2 ─┬─► M4 ─► M5
 │              │    ▲       ╲
 └─► M3 ────────┼────┤        ► M7 ─► M9
      │         └─► M6 ──────╱       ▲
      │         └─► M8 ──────────────┘
      └─► M10 (whenever a rig exists)
```

## Human-needed inputs

| Input | Needed by | Notes |
|---|---|---|
| Disc image of the target game (`.cue` + raw `.bin`, MODE2/2352) | M2 | The human supplies the path in `psxw.local.toml`. Never search the filesystem for it. Its sha1 is pinned in `inputs.lock`. |
| Approval of the emulator/BIOS choice if OpenBIOS can't boot Beetle PSX | M3 | Fallbacks are listed in M3; the human picks. |
| A replay of real play (input log) for the long session | M7 | Recorded by the human playing the native or browser build. |
| Webview shell decision (browser tab only, or Tauri) | M5 (optional task) | Defaulted to browser tab only (D46); the human may reopen it. |
| Choice of second game | M9 | |
| A hardware rig (PS1 with serial port, USB-serial, FreePSXBoot or Unirom) | M10 | Or a community member with one. |
| License for psx-wasm | Before any public release | |

## Repository map

```
inputs.lock              pinned repos, toolchains, Docker images, disc hashes
psxw.local.toml          (gitignored) machine-local paths: disc images, overrides
games/<id>/game.toml     everything game-specific
boundary/                SDK adapter policy (version- and object-keyed from M8)
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
| **Adapter** | A thin Rust function replacing an SDK function that touches hardware or BIOS internals. |
| **Device** | A runtime component speaking the hardware's vocabulary: gpu, spu, cd, sio, clock, gte. |
| **Vector** | One recorded device input (e.g. a draw batch plus the VRAM state before it). |
| **Golden** | A device target's result for a vector, with its provenance. |
| **Provenance** | Where a golden came from: `hardware` or `emulator:<name>@<commit>`, plus the BIOS hash and runner version. |
| **Verified** | Checked against a true oracle: original code, exact by construction, or hardware. |
| **Provisional** | Checked against an emulator golden. |
| **Unresolved** | No golden yet, or the emulator is a known-weak oracle for it. |
| **Replay** | A per-frame input log; with the same disc and build it determines a run exactly. |
