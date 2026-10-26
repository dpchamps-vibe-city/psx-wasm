# M3 — Oracle harness (Docker)

**Status:** not started
**Depends on:** M0. Parallel with M1–M2; the whole-game tasks (8–9) need
M2's runs. **Unlocks:** M4, M6, M7, M10.
**Design refs:** design: Oracles (device harness, per-device status,
whole-game oracles); decisions D21–D24.

## Goal

Build the device oracle: a dockerized, headless, pinned PlayStation
emulator that runs the same runner program a real console will run later.
Add the vector and golden infrastructure around it, and a whole-game RAM
comparison against psx-wasm.

The harness proves itself on vectors whose results are exact by
construction.

## Context

- **The device target is Beetle PSX,** Mednafen's PS1 core as a libretro
  core: the most accurate emulator available, though not fully
  cycle-accurate. Known weak spot: CD drive timing, which stays unresolved
  until hardware (D23).
- **Nothing comes from the user's machine.** Every tool is built from
  source in Docker at a pinned commit. The BIOS is OpenBIOS (from
  PCSX-Redux, open source), built in Docker.
- **The runner is our own small PS1 program,** built with PSn00bSDK (open
  source), which executes vectors directly on the chips. The same binary
  runs on hardware in M10.
- **Vectors and goldens are game-derived** and never committed. They live
  in `.oracle/` (D31).

## Scope

**In**
- **Docker images:**
  - `psxw-oracle-emu`: Beetle PSX, OpenBIOS, and our headless libretro
    host;
  - `psxw-psx-sdk`: PSn00bSDK plus a MIPS toolchain, for the runner. Later
    also mkpsxiso and psxavenc, to build test discs (used in M6).
- **`oracle/emulator-host/`:** a headless libretro frontend in C.
- **`oracle/runner/`:** the runner program. Vector kinds v1: GPU, GTE,
  SPU, BIOS. (CD and XA come in M6, using a generated test disc.)
- **`crates/psxw-oracle`:**
  - vector formats and canonical encoding;
  - the golden store with provenance;
  - target drivers (Docker emulator now, serial hardware later);
  - capture, compare and reporting;
  - synthetic vector generators;
  - the replay format bridge;
  - RAM-by-symbol diff.
- **The recorder interface** that devices call in M4/M6 to emit vectors.
  The recording hooks themselves land with each device.
- **CLI:**
  - `psxw oracle images build`
  - `psxw oracle selftest`
  - `psxw oracle capture`
  - `psxw oracle check`
  - `psxw oracle play`
  - `psxw oracle ramdiff`

**Out**
- device implementations (M4/M6);
- the hardware transport (M10);
- CD and XA vectors (M6).

## Design details

### Docker images (`oracle/docker/`)

- **Base images** pinned by digest (e.g. `debian:bookworm-slim@sha256:…`).
  Sources pinned by commit in `inputs.lock`, under `[oracle.beetle]`,
  `[oracle.openbios]` (the PCSX-Redux commit) and `[oracle.psn00bsdk]`.
- **`psxw-oracle-emu`:**
  - builds `beetle-psx-libretro` with the software renderer only (no
    hardware renderer);
  - builds OpenBIOS from PCSX-Redux `src/mips/openbios`, using Debian's
    `gcc-mipsel-linux-gnu`;
  - builds `oracle/emulator-host`;
  - the entry point is `psxw-emu-host`.
- **`psxw-psx-sdk`:** PSn00bSDK at a pinned release with a pinned MIPS
  toolchain (prebuilt release tarballs verified by sha256, or built from
  source if no pinned prebuilt is available). The entry point builds
  `oracle/runner`.
- **`psxw oracle images build`** builds both, records the image digests
  into `.oracle/images.json`, and verifies that the source commits match
  `inputs.lock`.
- **Containers run** with `--rm --network=none`. Inputs are mounted
  read-only: the runner EXE, vectors, the disc image (from
  `psxw.local.toml`) when needed. Output goes to a writable work dir.

### Emulator host (`oracle/emulator-host/`, C)

- **What it does:**
  - `dlopen`s the core;
  - implements the libretro environment callbacks: system directory with
    the BIOS, core options, logging, input;
  - runs `retro_run` frame by frame;
  - reads and writes main RAM through `retro_get_memory_data(SYSTEM_RAM)`.
- **Core options are fixed for accuracy:**
  - software renderer at 1× internal resolution;
  - no overclocking, no enhancements;
  - CD access set for maximum timing fidelity;
  - fast boot off unless needed (record the choice).

  All options are printed into the provenance.
- **Modes:**
  - `exe --exe runner.exe --vectors in.bin --out out.bin`: load the runner
    as content, run until the runner's mailbox says ready, then exchange
    vectors.
  - `play --cue disc.cue --replay r.bin --trace-u32 ADDR --dump-at F,… --out dir/`:
    run the original game with replayed pad input. Record the u32 at ADDR
    (the frame counter) every frame, and dump main RAM at the given
    frames.
- **Mailbox:** a fixed RAM region (default `0x801E_0000`, 64 KiB) with this
  header:

  `magic | version | state(ready / pending / done / error) | kind | payload_len | result_len`

  1. The runner sets `ready`.
  2. The host writes the vector payload and sets `pending`.
  3. The runner executes, writes the result, and sets `done`.
  4. The host polls once per `retro_run`.

  Large state (VRAM or SPU RAM images) is streamed in chunks over several
  exchanges.

### Runner (`oracle/runner/`, C on PSn00bSDK)

- **Bare metal.** It initializes the GPU, SPU and GTE directly, with no
  libgpu or libspu semantics beyond what's needed, and disables interrupts
  where vectors require determinism.
- **Vector kinds v1:**
  - **GPU_DRAW:**
    1. upload the VRAM initial state (rects);
    2. set draw and display state words (GP0 `E1`–`E6`);
    3. send the GP0 word list by DMA (linked-list or block, as recorded),
       waiting for idle;
    4. read the requested rects back through GP0 `C0` and DMA.
  - **GPU_TRANSFER:** CPU→VRAM, VRAM→CPU and VRAM→VRAM copies, with mask
    settings.
  - **GTE_OP:**
    1. load all data and control registers;
    2. execute the command word;
    3. read all registers back, including FLAG and LZCR.
  - **SPU_RUN:**
    1. upload the SPU RAM initial state;
    2. apply timestamped register writes (timestamp = sample index);
    3. run N samples;
    4. read back the SPU RAM capture buffers (CD L/R, voice 1, voice 3),
       the reverb work area, and the voice status (ENDX), key and envelope
       registers.
  - **BIOS_CALL:**
    1. set up memory;
    2. call A0/B0/C0 function n with arguments;
    3. capture return values and memory ranges.

    Only meaningful against a real BIOS, so it's provisional against
    OpenBIOS. It is mainly prepared for M10.
- **The vector C header** is generated from `psxw-oracle`'s Rust
  definitions by a build step, so the encoding can't drift.

### Vectors and goldens (`psxw-oracle`)

- **A vector** is a kind plus a canonical payload. Its id is the sha256 of
  the encoding. Large state blobs are stored separately by hash and
  referenced, so a VRAM image shared by 100 vectors is stored once.
- **Store layout:**

  ```
  .oracle/vectors/<id>.vec
  .oracle/blobs/<hash>
  .oracle/goldens/<target>/<id>.res
  .oracle/goldens/<target>/<id>.json
  ```

  `<target>` is e.g. `emu-beetle-<sha7>-openbios-<sha7>-runner-<ver>`.
  The JSON is the provenance: target, commits, BIOS sha1, core options,
  runner version, timestamp.
- **Verdict per (vector, device implementation):**
  - `verified`: exact by construction, or a hardware golden;
  - `provisional`: an emulator golden;
  - `unresolved`: no golden, or a kind marked emulator-weak (CD timing);
  - `failed`.
- **`psxw oracle capture [--kind K] [--missing]`** runs every vector
  without a golden through the target.
- **`psxw oracle check <game> --device D`** runs psx-wasm's implementation
  natively (Rust) on every archived vector of that device and compares it
  with the goldens. It writes the report and exits non-zero on `failed`.

### Exact-by-construction generators (the harness self-test)

These generate GPU vectors with closed-form expected results, using only
operations whose result is fully determined by the input data: fills;
VRAM→VRAM copies without masking; CPU→VRAM uploads read back; 1:1
textured rectangles (no blend, no dither, modulation neutral) with 4, 8
and 15-bit textures and colour tables; untextured flat rectangles.

`psxw oracle selftest` captures them in the emulator and requires equality
with the closed-form results. That validates the runner, the host, the
mailbox, the store and the encoding end to end. It also supplies the
"verified" subset for M4.

### Whole-game RAM oracle

- **Replay format** (`psxw-abi::replay`): a header (game id, disc sha1,
  pad type), then per-frame pad state. psx-wasm and the emulator host
  consume the same file.
- **Frame alignment.** `game.toml [oracle] frame_counter = "<symbol>"`
  names the game's own frame or tick counter. Both sides record its value
  every frame, and comparisons align on equal counter values, not wall
  frames. That absorbs the differences in CD loading time.
- **`psxw oracle ramdiff <game> --replay R`:**
  1. runs psx-wasm (M2 or later) and the emulator, both dumping RAM at
     aligned counter values;
  2. compares by symbol, using the address map for names and sizes;
  3. honours `game.toml [oracle] exclude_symbols` (each entry needs a
     reason, e.g. the sound driver until M6) and always excludes the stack
     region;
  4. reports the first differing symbols per aligned point.

## Tasks

1. [ ] Pin sources in `inputs.lock`: beetle-psx-libretro commit, PCSX-Redux
   commit (OpenBIOS), PSn00bSDK release and toolchain, base image digests.
2. [ ] `psxw-oracle-emu` Dockerfile: Beetle software build, OpenBIOS
   build, the emulator host. `psxw oracle images build`.
3. [ ] **BIOS gate.** Boot a trivial PS-X EXE under Beetle with OpenBIOS.
   - If Beetle refuses the BIOS or misbehaves, record the evidence in
     Findings.
   - Then evaluate, in order: Beetle options that relax BIOS checks; a
     minimal, documented Dockerfile patch that only affects BIOS
     identification, never emulation; a human-supplied BIOS from their own
     console.
   - Stop and ask the human before taking a fallback.
4. [ ] `psxw-psx-sdk` Dockerfile and the runner skeleton: mailbox, ready
   and echo vector.
5. [ ] `psxw-oracle` vector formats, store, provenance, C header
   generation, the target driver for Docker.
6. [ ] Runner kinds GPU_DRAW, GPU_TRANSFER, GTE_OP, SPU_RUN, BIOS_CALL;
   one handwritten vector of each through the whole pipeline.
7. [ ] Exact-by-construction generators plus `psxw oracle selftest`.
8. [ ] *(needs M2)* Replay format in `psxw-abi`; the emulator host's
   `play` mode; frame-counter tracing; RAM dumps; `psxw oracle play`.
9. [ ] *(needs M2)* `psxw oracle ramdiff`: psx-wasm side (the M2 host
   gains `--replay`, `--trace-u32` and `--dump-at`), alignment,
   by-symbol comparison, report.
10. [ ] Recorder interface (`psxw-oracle::record`) that devices call; the
    per-device hooks are done in M4/M6.
11. [ ] Add the oracle self-test and image checks to `psxw ci`. Update
    docs and the README status.

## Exit criteria (all automated)

1. `psxw oracle images build` builds both images from pinned sources.
   Rebuilding yields the same commits, and the digests are recorded.
2. `psxw oracle selftest` passes: at least 1,000 generated
   exact-by-construction GPU vectors, all matching their closed-form
   results.
3. GTE, SPU and BIOS handwritten vectors are captured, with provenance.
   Capturing twice gives byte-identical results (emulator determinism).
4. *(needs M2)* `psxw oracle play btn --replay <manifest replay>
   --dump-at …` is deterministic across two runs.
5. *(needs M2)* `psxw oracle ramdiff btn --replay <manifest replay>` runs
   end to end and writes a symbolized report. Equality isn't required
   until M4.
6. The BIOS decision is recorded in Findings (OpenBIOS works, or the
   fallback approved by the human).

## Risks and decision points

- **The BIOS gate (task 3)** is the main unknown. Stop and ask if
  OpenBIOS fails.
- **Beetle's PS-X EXE side-loading** must work. If not, wrap the runner in
  a minimal ISO built with mkpsxiso, booted as a disc.
- **Mailbox polling granularity** is one frame. Use chunking for large
  state. Throughput is fine for golden creation, which is offline.
- **Emulator determinism** must hold for the provenance to mean anything.
  If a core option introduces nondeterminism, fix the option.

## Progress log

## Findings
