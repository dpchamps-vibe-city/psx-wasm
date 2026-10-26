# M3 — Oracle harness (Docker)

**Status:** in progress (tasks 1–7, 10, 11 done; 8–9 wait for M2)
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
    vectors. *As built:* `exe --disc psxw-runner.cue …`, because Beetle's
    EXE loader cannot start a program under OpenBIOS (Findings); `--exe`
    remains for other BIOSes.
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

  *As built* (`crates/psxw-oracle/src/wire.rs`): every exchange is a
  batch of runner ops (ECHO, INFO, GPU_RESET, VRAM_WRITE, VRAM_READ,
  GPU_EXEC, GTE_EXEC, SPU_RESET, SPU_RAM_WRITE, SPU_RUN, BIOS_CALL), and
  the result holds one status and result per op. The driver
  (`psxw-oracle::script`) compiles each vector kind into ops, chunks large
  state, and packs as many ops into each exchange as fit. The header also
  carries `seq`/`ack_seq`, the runner and wire versions, a heartbeat, and
  two diagnostic fields naming the last device wait that timed out.

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

1. [x] Pin sources in `inputs.lock`: beetle-psx-libretro commit, PCSX-Redux
   commit (OpenBIOS), PSn00bSDK release and toolchain, base image digests.
2. [x] `psxw-oracle-emu` Dockerfile: Beetle software build, OpenBIOS
   build, the emulator host. `psxw oracle images build`.
3. [x] **BIOS gate.** Boot a trivial PS-X EXE under Beetle with OpenBIOS.
   (Outcome: OpenBIOS works; EXE side-loading doesn't, so the runner boots
   from a disc as the risks section prescribes. See Findings.)
   - If Beetle refuses the BIOS or misbehaves, record the evidence in
     Findings.
   - Then evaluate, in order: Beetle options that relax BIOS checks; a
     minimal, documented Dockerfile patch that only affects BIOS
     identification, never emulation; a human-supplied BIOS from their own
     console.
   - Stop and ask the human before taking a fallback.
4. [x] `psxw-psx-sdk` Dockerfile and the runner skeleton: mailbox, ready
   and echo vector.
5. [x] `psxw-oracle` vector formats, store, provenance, C header
   generation, the target driver for Docker.
6. [x] Runner kinds GPU_DRAW, GPU_TRANSFER, GTE_OP, SPU_RUN, BIOS_CALL;
   one handwritten vector of each through the whole pipeline.
7. [x] Exact-by-construction generators plus `psxw oracle selftest`.
8. [ ] *(implemented; the btn run waits for the disc)* Replay format in `psxw-abi`; the emulator host's
   `play` mode; frame-counter tracing; RAM dumps; `psxw oracle play`.
9. [ ] *(implemented; the btn run waits for the disc)* `psxw oracle ramdiff`: psx-wasm side (the M2 host
   gains `--replay`, `--trace-u32` and `--dump-at`), alignment,
   by-symbol comparison, report.
10. [x] Recorder interface (`psxw-oracle::record`) that devices call; the
    per-device hooks are done in M4/M6.
11. [x] Add the oracle self-test and image checks to `psxw ci`. Update
    docs and the README status. (`psxw ci` doesn't exist yet: the gate is
    `psxw oracle ci` = `psxw_oracle::commands::ci`, for `psxw ci` to call;
    see Findings.)

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

- **2026-10-26** (`2483fcf`): pins in `inputs.lock` (`[oracle]`, parsed by
  `psxw-inputs::lock`); `oracle/docker` (`psxw-oracle-emu`,
  `psxw-psx-sdk`); `oracle/emulator-host` (`psxw-emu-host`);
  `oracle/runner` with every v1 op; `psxw-oracle::wire` and the generated
  `oracle/include/psxw_oracle_wire.h`. BIOS gate: OpenBIOS works, EXE
  side-loading doesn't, so the runner boots from a disc (Findings).
- **2026-10-26** (`9e869a2`): the `psxw-oracle` crate (vectors, stores,
  provenance, verdicts, script compilation and packing, Docker target,
  exact-by-construction generators, handwritten vectors, reports,
  recorder) and the CLI (`crates/psxw-cli/src/oracle.rs`). Runner fixes
  found by the selftest: per-packet GP0 pacing, DMA uploads, diagnostic
  mailbox fields.
- **2026-10-26** (`ab7d061`): `images build --no-cache`; core and host
  hashes in the image sources and in provenance; docs. Results, all from
  `psxw oracle …`:
  - `images build --no-cache` rebuilt both images from scratch: same
    commits, and bit-identical OpenBIOS (sha1 `8b344d0d…`), Beetle core
    (sha256 `a24f8a70…`), host (`76b44760…`), runner EXE (`ae1204b7…`)
    and runner disc (`628f75b7…`); only the local image ids differ.
  - `selftest`: 1,050 vectors (seed `0x505358574d330001`, 150 per
    generator; GP0 by port 298, block 293, linked list 309), all
    verified; about 4 s including boot (44 exchanges of one frame each).
  - `capture --missing` then `capture`: 24 handwritten vectors (13 GTE,
    2 SPU, 5 BIOS, 3 GPU, 1 echo) captured with provenance; every one of
    the 1,074 stored vectors reproduced byte for byte, across a
    from-scratch image rebuild and across different batch compositions.
  - `check btn --device gpu` reports the 1,053 GPU vectors unresolved
    (no implementation yet) and exits non-zero; `ci` passes.
  - `cargo test --workspace` passes (the Docker test skips when Docker or
    the images are missing).

- **2026-10-26** (`a56369d`): tasks 8–9 implemented (D39).
  - `psxw-emu-host play` reads replays with Beetle's own pad table
    (`input.c` `input_map_controller`, checked at the pinned commit),
    traces a word every frame, and dumps RAM by frame and by counter value.
  - `psxw oracle play` mounts the user's disc read-only; `ramdiff` aligns
    and compares by symbol.
  - btn's counter is `D_8006C838+0x8`: `RunSceneUpdateLoop` adds one every
    scene frame, and the boot-time `memset` zeroes it.
  - `play_traces_and_dumps_deterministically` (`tests/docker.rs`) plays the
    runner disc twice: the trace and both dump kinds match exactly. The
    rebuilt emulator image changed only the host binary; `oracle ci` still
    reproduces all 1,074 goldens.

## Findings

### The BIOS gate (exit criterion 6): OpenBIOS works; the runner boots from a disc

- **Beetle accepts OpenBIOS as is.** The core option
  `beetle_psx_override_bios = openbios` loads `openbios.bin` from the
  system directory with no SHA-1 check (log: "Override firmware found …
  SHA1 8B344D0D…"). No option relaxing BIOS checks and no identification
  patch is needed. OpenBIOS is built from PCSX-Redux `8a86b7e` (nugget
  `4dc70d2`) with Debian's `gcc-mipsel-linux-gnu` 14.2.0, in the default
  configuration (ROM, shello shell, Release).
- **Beetle's PS-X EXE side-loading cannot start a program under
  OpenBIOS.** Evidence:
  - Beetle's `LoadEXE` writes `jal 0xBF001000` at BIOS offset `0x6990`
    (where the Sony BIOS calls its shell) and puts a loader in expansion-1
    memory.
  - In this OpenBIOS build, `0xBFC06990` is the delay slot of `jr ra` in
    `loadExeHeader`, which a boot without a disc never executes.
  - Loading the runner as EXE content for 1,800 frames: the mailbox never
    came up, the runner's text never reached RAM (0x80010000 stayed
    zero), and OpenBIOS was running its own shell, copied to 0x80030000.
- **Options evaluated, in the plan's order.** Beetle options relaxing
  BIOS checks: not applicable, because OpenBIOS is already accepted and no
  option moves the EXE hook. A patch touching only BIOS identification:
  not applicable, for the same reason. A human-supplied BIOS: excluded.
  So the risks section's remedy applies: **the runner is wrapped in a
  minimal disc built with mkpsxiso** (bundled with PSn00bSDK v0.24) and
  OpenBIOS boots it from CD like a game. Ready after 445 frames. Neither
  Beetle nor OpenBIOS is changed.
- **Disc identification.** Beetle models the CD controller's licence
  check: data reads fail unless it identifies the disc as licensed, from
  the SYSTEM.CNF boot file name (an `S[CLI]xx_` serial) or licence text in
  sector 4. The runner disc names its boot file `SLUS_000.00` (no real
  product). This is identification only: no Sony data is on the disc.
  **For the human to review:** if serial-style naming is unwelcome, the
  alternative is an OpenBIOS build with its upstream `EMBED_PSEXE` option
  set to a small loader that takes the runner over the mailbox. That needs
  no disc identification, but changes the BIOS image.

### Core options (printed in every golden's provenance)

- **Beetle is built** with `HAVE_HW=0` (software renderer only) and
  `HAVE_LIGHTREC=0` (interpreter only). `HAVE_CHD=0` does not link at
  this commit, so the CHD disc reader stays in; it is not emulation.
- **Fixed, required** (the host refuses to run if the core stops
  declaring one): `cpu_freq_scale=100%`, `gpu_overclock=1x(native)`,
  `gte_overclock=disabled`, `internal_resolution=1x(native)`,
  `dither_mode=1x(native)`, `pgxp_mode=disabled`,
  `widescreen_hack=disabled`, `cd_access_method=sync`,
  `cd_fastload=2x(native)`, `override_bios=openbios`, `skip_bios=disabled`,
  `region=ntsc-u`, plus three that matter more than they look:
  - `line_render=disabled`: the default turns on a Line-to-Quad hack in
    the software renderer;
  - `spu_silent_voice=disabled`: by its own description the default
    leaves SPU register state inexact;
  - `compatibility_settings=disabled`: per-game overrides of the above.

  `skip_bios` must stay off: its patch writes the same Sony-BIOS address
  0x6990 and would corrupt OpenBIOS. Fast boot is therefore off.
- **This Beetle master has an "idle virtualisation" optimisation** (GPU
  and DMA events become virtual while idle). Results are deterministic in
  every test so far. If timing-sensitive vectors (CD, SPU IRQ)
  misbehave in M6, look here first.

### Reproducibility and provenance

- **Builds are bit-reproducible** from the pins: OpenBIOS, the Beetle
  core, the host and the runner EXE and disc are identical after a
  no-cache rebuild. Apt packages come from the snapshot the base image
  itself was built from (`20261020T213255Z`).
- **Image ids are local** (layers carry build timestamps). They are
  recorded in `.oracle/images.json` and in each golden; `images verify`
  checks them against the daemon, and the commits against `inputs.lock`.
- **Target id:** `emu-beetle-7fea57a-openbios-8a86b7e-runner-1`.
  `RUNNER_VERSION` (`wire.rs`) must be bumped whenever a runner change can
  alter results.

### Runner and protocol facts later milestones need

- **The protocol is batches of ops** (`wire.rs`). The driver compiles
  vector kinds into ops, chunks large state and packs exchanges; one
  exchange takes one frame. Every vector starts with its device's reset op
  (`GPU_RESET`, `SPU_RESET`), so results do not depend on what ran before
  it. This was checked: identical results in different batch compositions.
- **GP0 pacing.** GPUSTAT bit 28 ("ready for a DMA block") drops as soon
  as the first word of a command is queued, and bit 25 means nothing in
  DMA mode 1. So waiting per word deadlocks.
  - The runner waits for bit 26 (idle) before each packet.
  - Port and block packets are at most 16 words (the FIFO depth);
    linked-list nodes at most 255.
  - Each packet holds whole commands.
  - VRAM uploads send the header by port and the pixels by DMA in blocks
    of 16 words, as `LoadImage` does.
- **GPU_EXEC** clears the texture cache (GP0 01h) before the state words.
- **SPU:**
  - SPU_RUN starts at a SPUSTAT bit 11 edge. Beetle reports bit 11 only
    while transfer control is 0004h, which SPU_RESET sets.
  - SPU_RESET leaves ENDX = 00FFFFFFh and reserves SPU RAM 1000h–100Fh.
  - Captures are DMA-read right after the register snapshot while the
    SPU keeps running. Deterministic, but provisional by design: M6
    refines it.
- **BIOS_CALL** works with OpenBIOS's kernel fully initialised (the
  runner is booted by the BIOS itself). It is provisional, as planned.
- **Diagnostics.** A timed-out device wait is recorded in the mailbox
  header (`MBX_OFF_DIAG_*`) and logged by the host
  (`out/oracle/runs/last-host.log`).

### Integration points

- **`psxw ci` does not exist yet.** The oracle's part is `psxw oracle ci`
  (`psxw_oracle::commands::ci`): images verify, selftest, capture with
  determinism. Whoever adds `psxw ci <game>` should call it.
- **Device implementations** register in
  `crates/psxw-cli/src/oracle.rs::devices()` (trait `DeviceUnderTest`).
  `check` judges against exact, then hardware (`hw-…` target dirs), then
  this pin's emulator goldens.
- **Tasks 8–9 fit as designed.** The host stubs the `play` mode;
  `input_state` and per-frame RAM reads are the hooks it needs, RAM dumps
  go to the work dir, and `psxw oracle play`/`ramdiff` belong in
  `commands.rs` next to `capture`.
- **Disk.** The images are 92 MB (emu) and 370 MB (sdk); a no-cache
  rebuild needs about 2 GB of build cache.
