# M4 — Pixels

**Status:** in progress (tasks 1–2: psx-gte and its oracle; the runtime
wiring of both is pending)
**Depends on:** M2, M3. **Unlocks:** M5, M7.
**Design refs:** design: Runtime (gpu, gte, sio), §5 (adapters), Oracles
(per-device status); decisions D12, D13, D20–D24.

## Goal

The game renders, byte-exact at 1×, from the GTE through to VRAM.

- **`psx-gte`** is exact.
- **The GPU front end** (in the runtime) and **the WebGL2 backend** (in
  the host, Rust over `glow`) reproduce VRAM byte for byte at 1×.
- **The pad works.**
- **libgpu, libetc and libpad adapters** replace their hardware-touching
  functions.

Every piece is gated by an oracle: emulator goldens (provisional),
exact-by-construction vectors (verified), and conformance against the
original libraries (original code).

## Context

- M2 provides the linked, booting game with stub GPU, SPU and pad
  adapters, the scheduler and callbacks.
- M1's hardware-access report lists exactly which SDK functions touch GPU
  or pad registers. Those, and only those, get adapters. Example: PsyQ's
  `ClearOTagR` clears the ordering table with DMA channel 6, so it is
  hardware-touching even though it looks pure.
- M3 provides the vector store, the emulator target, the
  exact-by-construction generators and `ramdiff`.
- From M3's Findings:
  - register the GPU and GTE implementations in
    `crates/psxw-cli/src/oracle.rs::devices()` (trait
    `psxw_oracle::commands::DeviceUnderTest`); `psxw oracle check` then
    judges them against exact, hardware and emulator goldens;
  - record through `psxw_oracle::record::Recorder` into the set
    `game-<id>`; `record::vram_rect` snapshots VRAM for GPU_DRAW's initial
    state;
  - GPU packets must hold whole commands: port and block packets at most
    16 words, linked-list nodes at most 255 (`Vector::check` enforces). A
    GPU vector starts with GP1 reset, its VRAM rectangles, a texture-cache
    clear (GP0 01h), then its recorded state words;
  - the 1,050 selftest vectors (set `selftest`, closed-form goldens under
    `.oracle/goldens/exact`) are the verified subset.
- btn facts (for scale, not code):
  - mostly sprites, plus GTE-projected polygons;
  - `StoreImage` at 8 call sites, `MoveImage` at 13;
  - no MDEC and no 24-bit display;
  - DualShock rumble through `PadSetAct`.

## Scope

**In**
- **`psx-gte`:**
  - the register file: IR saturation; the SXY and SZ FIFOs;
    LZCS→LZCR; IRGB/ORGB conversion; FLAG with bit 31 as the summary;
  - all commands with their `sf`/`lm`/`mx`/`v`/`cv` fields, including the
    hardware quirks (e.g. MVMVA with far-colour translation);
  - RTPS/RTPT division through the UNR table, exactly per psx-spx;
  - the intrinsic API used by generated C and by adapters.

  Its state lives in the runtime-private page of the window.
- **GPU front end (`psx-devices::gpu`):**
  - draw state (draw area, offset, texpage, texture window, dither,
    mask set and check, draw-to-display);
  - display state;
  - a GP0 packet decoder for every primitive type: polygons (flat,
    Gouraud, textured, semi-transparent, 3 and 4 vertices), lines and
    polylines, rectangles and sprites (variable, 1×1, 8×8, 16×16), fills,
    the three transfer kinds, and the environment commands;
  - the GP1 display commands reached through `PutDispEnv` and related
    adapters;
  - batching; tracking of VRAM read and write regions for overlap splits;
  - the renderer command stream.
- **Renderer command interface** (`psxw-abi::render`):
  - `SetState`, `Draw(batch)`, `Fill`, `Upload`, `Copy`, `Readback`
    (synchronous), `Present`;
  - host imports `render_submit(ptr, len)`,
    `render_readback(x, y, w, h, dst)` and `present(desc_ptr)`.
- **`psx-render-gl`,** the backend in Rust over `glow`, as specified
  below. It gets a native headless context through EGL surfaceless (Mesa
  llvmpipe in Docker, for CI determinism) and an optional native window.
- **Adapters** in `sdk-psyq`:
  - libgpu's hardware-touching functions (expected: `ResetGraph`,
    `DrawOTag`, `DrawOTagEnv`, `DrawPrim`, `DrawSync`, `DrawSyncCallback`,
    `LoadImage`, `StoreImage`, `MoveImage`, `ClearImage*`, `ClearOTagR`,
    `PutDrawEnv`, `PutDispEnv`, `SetDispMask`, `GetODE`, …, confirmed by
    the report);
  - libetc: `VSync` modes, the callbacks, `GetVideoMode`/`SetVideoMode`;
  - libpad (`PadInitDirect`, `PadStartCom`, `PadGetState`, `PadInfo*`,
    `PadSetAct*`, `PadSetMainMode`, …);
  - libapi's pad patch members (`InitPAD`, `StartPAD`, `_patch_pad`,
    `ChangeClearPAD`, `_remove_ChgclrPAD`).
- **The SIO pad device:** digital and DualShock reports (analog, rumble
  state), per-game pad type in `game.toml`.
- **Vector recording hooks:**
  - GPU: each `DrawOTag`, `DrawPrim` or transfer becomes a vector, with
    the VRAM state before it as a referenced blob;
  - GTE: each distinct (command, input registers).
- **Adapter conformance** against the translated original libraries,
  through test-only register decoders: GP0/GP1 port and DMA linked-list
  walks for libgpu, SIO0 byte exchanges for libpad.
- **Replays** in `games/<id>/replays/`. They are input logs, which are
  committable. `title-to-gameplay` reaches early gameplay. Author it with
  scripted inputs, iterating on headless frame dumps, or have the human
  record one through the native window.

**Optional in this milestone**
- **The `asm-gte` normalizer rule:** map PsyQ's GTE inline-asm macro
  templates (`mtc2`/`lwc2`/`cop2` sequences) onto `psx_gte_*` calls, so
  GTE-using C compiles as C instead of being lifted. Gated by `difftest`.

**Out**
- upscaling and other enhancements (later, unverified);
- the browser host (M5);
- MDEC.

## Design details

### WebGL2 backend (byte-exact at 1×)

- **VRAM:**
  - `vram_w`, a 1024×512 `R16UI` render target holding the exact pixel
    values;
  - `vram_r`, an `R16UI` texture used as the read copy;
  - an explicit copy from `vram_w` to `vram_r` brings a region up to date.
- **Per primitive:**
  - draw a covering quad: the bounding box clipped to the draw area, at
    integer pixel centres;
  - the fragment shader evaluates, in integers, the PS1's edge functions
    and fill rules (the coverage decision) and discards uncovered pixels;
  - it then computes Gouraud colour and texture coordinates with the PS1's
    fixed-point interpolation;
  - textured primitives apply the texture window, then fetch from `vram_r`
    at 4, 8 or 15 bits per pixel, through the colour table for 4 and 8;
  - texel `0x0000` is transparent;
  - modulated texels use `(texel × colour) >> 7` with saturation; raw
    texels skip modulation.
- **Semi-transparency:**
  - mode B/2+F/2, B+F, B−F or B+F/4 on 5-bit channels with the hardware's
    rounding and clamping;
  - applied per pixel when the primitive is semi-transparent and, for
    textured primitives, the texel's STP bit is set;
  - the destination B is read from `vram_r`.
- **Dither:** the 4×4 table, applied when enabled for Gouraud or modulated
  textured primitives, before truncating to 5 bits.
- **Mask:** "check mask" skips pixels whose destination has bit 15 set,
  read from `vram_r`; "set mask" forces bit 15 on output.
- **Lines:** PS1 line rasterization rules, done per pixel in the shader on
  a thin covering quad.
- **Rectangles and sprites:** no Gouraud; texture flip bits from draw
  mode (`E1` bits 12–13) where the GPU supports them.
- **Ordering and hazards:**
  - primitives execute in order;
  - before a primitive that reads (texture, blend or mask) any region
    written since the last read-copy sync, copy those regions from
    `vram_w` to `vram_r`;
  - batches are maximal runs with no such hazard;
  - correctness first, with hazard tracking at bounding-box granularity;
    optimize later.
- **Transfers:**
  - uploads honour mask settings and are rendered as quads from a staging
    texture;
  - VRAM→VRAM copies honour mask settings, through the shader;
  - fills ignore the mask and clip per the hardware;
  - readback uses `readPixels` with `RGBA_INTEGER` on `vram_w`.
- **Present:** convert the display area from 15-bit (or 24-bit, for
  generality) into an RGBA8 output texture, then a presentation pass to
  the canvas or window.

### Front end to backend contract

The front end resolves all PS1 state into explicit per-primitive
attributes: vertices, colours, UVs, CLUT position, texpage, blend mode,
mask flags, dither and window. The backend is a pure function from
commands and the VRAM state to a new VRAM state. That makes
`psxw oracle check --device gpu` possible: replay a vector through
front end plus backend, read the VRAM back, and compare it with the
golden.

## Tasks

1. [x] `psx-gte`: register file and all commands, plus unit tests from
   spec examples (debug aids). Wire it into the runtime's private page and
   the intrinsics. *(psx-gte is done; the wiring into `psx-runtime` is
   pending, with M2's runtime work: see Findings.)*
2. [x] GTE recording hook. Synthetic GTE edge vectors: saturation
   boundaries, division overflow, every command with `sf`/`lm`
   combinations. Run `psxw oracle capture` for GTE, then
   `psxw oracle check --device gte`. *(The hook is
   `psxw_oracle::gte::GteHook`; the runtime does not call it yet, so there
   are no recorded GTE vectors.)*
3. [ ] GPU front end: packet decoder for every type, state, renderer
   command emission. Unit tests decode every packet type.
4. [ ] Renderer command interface in `psxw-abi`; host imports in the
   native host.
5. [ ] `psx-render-gl` with a headless EGL context (Docker image with Mesa
   llvmpipe, pinned). Implement fill, transfers and copies first, then
   `psxw oracle selftest` against the backend. That gives the verified
   subset.
6. [ ] Primitives: sprites and rectangles, then polygons, then lines;
   blending, dither and mask; hazard tracking. Check each feature against
   emulator goldens from synthetic vectors before recorded ones.
7. [ ] libgpu adapters (classification from M1's report) and the libgpu
   conformance harness (register decoder).
8. [ ] libetc adapters (`VSync` modes 0, 1, n, −1; callbacks) and
   conformance.
9. [x] SIO pad device; libpad and libapi pad adapters; conformance (SIO0
   decoder); replay input path; optional native window input. *(D40:
   libpad runs as original code over the SIO0 device, so it has no
   adapters; `psxw conform btn --lib libpad` passes. libapi's pad members
   serve the BIOS pad driver, which the HLE BIOS implements. The optional
   window input comes with the native window.)*
10. [ ] GPU recording hook. Author the `title-to-gameplay` replay; record
    vectors; capture goldens; reach zero failures.
11. [ ] `psxw oracle ramdiff btn --replay title-to-gameplay`, with
    `exclude_symbols` covering only sound-driver state (with reasons).
    Reach zero differences.
12. [ ] *(optional)* `asm-gte` normalizer rule; `difftest` on the
    functions that move to C.
13. [ ] `psxw run btn --headless --replay … --dump-frames out/` writes
    PNGs for humans (not gating). Add the GPU and GTE checks to `psxw ci`.
    Docs, README status. *(The GTE check is in: `psxw_oracle::commands::ci`
    runs it for every game.)*

## Exit criteria (all automated)

1. `psxw oracle check btn --device gte`: every recorded and synthetic GTE
   vector matches its golden (provisional). Zero failed.
2. `psxw oracle check btn --device gpu`:
   - the exact-by-construction subset is 100% verified;
   - every vector recorded from `title-to-gameplay` is byte-exact against
     its golden at 1× (provisional);
   - zero failed or unresolved.
3. Adapter conformance passes for libgpu, libetc and libpad, against
   their translated originals, on all recorded call vectors.
4. `psxw oracle ramdiff btn --replay title-to-gameplay` reports zero
   differing symbols at every aligned point, outside the documented
   sound-driver exclusions.
5. Determinism: frame (VRAM) hashes and RAM hashes are identical across
   two native runs of the replay.
6. `psxw ci btn` includes 1–5.

## Risks and decision points

- **Interpolation exactness** (Gouraud and UV rounding) is the classic
  hard part. Iterate against synthetic vectors that isolate one feature
  at a time.
- **Hazard copies** may be slow for heavy blending. Measure, then
  optimize: finer-grained regions, or a tile-based approach.
- **Headless GL differences across drivers.** The shaders are
  integer-only, so the results should be identical. Verify Mesa llvmpipe
  against a real GPU early, and make any difference a Finding.
- **Authoring the replay without seeing the game.** Use frame dumps; ask
  the human to record one if scripting stalls.

## Progress log

- **2026-10-26** (`8bb05c3`): `psx-gte`, exact per psx-spx: the register
  file with its read and write behaviour, every command with `sf`, `lm`
  and MVMVA's fields, the UNR division, FLAG; `no_std`, builds for
  wasm32; 11 spec-derived unit tests (debug aids).
- **2026-10-26** (`6c3ba64`): the oracle side, in `psxw-oracle::gte`:
  psx-gte as the `gte` device (`psxw oracle check <game> --device gte`),
  the synthetic generators, the disputed set, the recording hook
  `GteHook`, and the GTE check in `commands::ci`. Results, all from
  `psxw oracle …`:
  - `capture --kind gte_op`: 5,543 synthetic vectors (seed
    `0x505358574d340001`: commands 2,912, division 953, saturation 656,
    fifo 430, orgb 200, registers 200, lzcr 192) plus the 13 handwritten
    GTE vectors and the 6 disputed ones, captured with provenance on
    `emu-beetle-7fea57a-openbios-8a86b7e-runner-1`. A full recapture
    reproduced all 7,252 stored GTE vectors byte for byte.
  - `check btn --device gte`: 5,556 vectors, 5,556 provisional, 0
    unresolved, 0 failed. The first run failed 54 (50 AVSZ, 4 GPL); both
    are settled in Findings.
  - `ci`: images verify, selftest (1,050 verified), capture (8,313
    vectors, 8,302 reproduced byte for byte, 11 new) and the GTE check
    all pass.
  - Mutation check (a throwaway harness, not committed): 21 deliberate
    mistakes in psx-gte, from dropping the 44-bit wrap to an off-by-one in
    the UNR index, were each caught, by 9 to 2,419 vectors. RTPT's
    last-vertex-only depth cue was caught by 13 once a targeted group was
    added, and the far colour's dropped-sum overflow flags by 37.
  - `cargo test --workspace --release` passes.

- **2026-10-26** (`b7f719e`, `ff17bea`, this commit): adapter conformance
  and the pad (D40).
  - **The harness:** `psxw conform <game> --lib L` runs one scenario of SDK
    calls and vblanks without a disc, on the adapter build and on
    `game-orig-L.wasm`, and compares return values and the SDK buffers
    after each step.
  - **I/O emulation:** the link's I/O hook passes the access kind and
    runs after stores; `psx-bios::io` is the register bus.
  - **SIO0:** a bit-level port of Beetle's FrontIO and gamepad.
  - **IRQ chains:** they run as OpenBIOS runs them.
  - **What the first run found:**
    - libpad's IRQ helpers are reached only through a data table, so
      the lift now starts functions at data pointers to unreachable code
      (21 in btn);
    - a hoisted load spun forever on JOY_STAT, so lifted loop heads now
      carry a memory barrier;
    - the libpad adapters differed from the original in their init bytes,
      PadGetState's sequence and PadStopCom's result.

    Hence D40.
  - **Results:** `psxw conform btn --lib libpad --frames 120` passes, 124
    steps, with the pad's button bytes identical frame for frame. The M1
    difftest still passes (2,566 + 3 excluded), with the translated side
    given 4x fuel.

## Findings

### Where psx-spx is open, and what the device oracle settled

psx-gte implements psx-spx. Where its text is ambiguous, the emulator
goldens decided, and the code marks those places "oracle:".

- **AVSZ3/AVSZ4's OTZ comes from the untruncated product.** psx-spx says
  "OTZ = MAC0/1000h (saturated to 0..FFFFh)". When ZSF×ΣSZ overflows 32
  bits, OTZ saturates the way the full product's sign says (FFFFh or 0,
  with FLAG.18), not from MAC0's wrapped 32 bits. That was 50 of the
  first run's 54 failures. Beetle implements it as "MAC0 overflow forces
  OTZ to saturate", which is equivalent.
- **RTPS/RTPT use the 44-bit sum, not the 32-bit registers.** SZ3 and
  FLAG.22 come from the Z sum SAR 12, which differs from psx-spx's
  "MAC3 SAR ((1-sf)*12)" when sf=0 and the sum passes 32 bits. SX2/SY2
  come from the full OF + IR×q, before MAC0 truncates it. IR0/MAC0 are
  depth-cued for RTPT's last vertex only.
- **The intermediate steps saturate from MAC's 32 bits after SAR sf.**
  That covers the far-colour bug's dropped partial sum and the depth-cue
  "(FC−MAC)" IR. Both are checked as lm=0 and only raise flags (the far
  colour's 44-bit overflow too).
- **N/A opcodes.** The emulator runs 00h as RTPS and 1Ah as DCPL (Mednafen's
  "alternate?" cases). psx-spx says 00h "modifies similar registers than
  RTPS" and doesn't list 1Ah. psx-gte follows the oracle, and 32 vectors
  check it. The other N/A opcodes clear FLAG and do nothing else, as in
  the emulator. They have no vectors: psx-spx calls them unknown, and
  some are reported unstable on hardware.
- **Writes are never saturated.** The plan's "IR saturation on writes" is
  the saturation commands apply when they write IR. `mtc2` to a 16-bit
  register keeps the low half and sets no flag (psx-spx's note; the
  `registers` generator checks it with garbage in every upper half).

### Disputed: GPL's MAC overflow flag at exactly 2^43−1

- **What disagrees.** With sf=1 and MAC SHL 12 + IR×IR0 = 2^43−1 exactly,
  the emulator sets FLAG.30–28 (MAC positive overflow). psx-spx defines
  the flag as "larger than 43 bits", and psx-gte flags only at 2^43.
  The emulator's own MVMVA and RTPS paths also flag only at 2^43, and
  the 44-bit boundary vectors above agree with them.
- **The cause.** Beetle's `mednafen/psx/gte.c` routes GPL, DPCS/DPCT,
  DCPL and INTPL through `i64_to_i44`, which tests
  `value >= 0x7ffffffffff`, while `A_MV` tests `value >= 1 << 43`. Only
  GPL can reach 2^43−1 exactly: the other commands' sums are multiples of
  16, or bounded far below. It looks like an off-by-one in the emulator.
- **Settled for now in favour of psx-spx.** The six vectors are kept in
  the set `disputed-gte`. They are captured like any other, but kept out
  of the verdicts, and every `check --device gte` report says so in a
  note ("psx-gte matches the emulator on 0"). The generators avoid
  exactly 2^43−1 in GPL. Hardware (M10) settles it. If hardware flags,
  change GPL's accumulation step only.
- **Impact.** A game would need a GPL accumulation landing on that one
  value.

### What GTE_EXEC cannot reach (covered by unit tests only)

- **Three behaviours are never exercised.** The runner never writes SXYP
  (15) or IRGB (28), and every vector runs a command, which clears FLAG.
  So SXYP's move-on-write, IRGB's expansion into IR1..3, and FLAG writes
  (bits 30..12 kept, bit 31 recomputed) are covered only by psx-gte's
  spec unit tests: unverified. Reads are covered: SXYP, IRGB/ORGB, LZCR
  and FLAG are read back in every vector.
- **Reaching them needs a runner change.** Flags in GTE_OP's unused cmd
  bits 25..31 ("also write SXYP", "also write IRGB", "no command") would
  keep the vector encoding. But they need a `RUNNER_VERSION` bump and a
  regenerated wire header. The header is copied into the emulator image,
  so the change rebuilds `psxw-oracle-emu:local` for every checkout
  (M3 Findings). Deferred to the next runner change.

### Decisions

- **The state is 64 words in hardware numbering.** Each word holds what
  `mfc2`/`cfc2` returns, and the derived registers are kept current. The
  runtime can therefore keep it in the runtime-private page
  (`Gte::view_mut`), and a recorder can copy it. `GteRegs` stays as an
  alias, because psx-runtime uses it.
- **Generated sets.** The synthetic vectors live in
  `synthetic-gte-<generator>`. `capture` and `check --device gte`
  regenerate them from the default seed and replace the sets
  (`Store::replace_set`), so a changed generator leaves no stale
  members. `check` includes every `synthetic-*` set and reports
  `disputed-*` sets in a note.
- **Unresolved fails the GTE check.** `check --device gte` fails on
  unresolved vectors too (`UNRESOLVED_FAILS`), because exit criterion 1
  wants every GTE vector matched.
- **`psxw ci` covers the GTE check.** `commands::ci` runs
  `check <game> --device gte` for every game in `inputs.lock`, with no
  change to `psxw-cli`'s `ci.rs`.

### For the runtime (M2's `psx-runtime`; not wired here)

- **Wire psx-gte in.** `psx-runtime` still stores raw words
  (`gte.data[r] = v`) and only counts `gte_exec`. It should call
  `Gte::mtc2/mfc2/ctc2/cfc2/exec`, with the `Gte` in the runtime-private
  page.
- **Call the recording hook.** Recording means calling
  `GteHook::before_exec(rec, &gte, cmd)`, or `before_exec_raw` with the
  64 words, just before each command. It records canonical vectors
  (registers as read back, FLAG 0, the imm25), deduplicated, at most
  `per_command` per command word, into a `Recorder` (a `StoreRecorder`
  on `game-<id>`). `game.wasm` cannot hold a `StoreRecorder`, so the
  native host has to do it, e.g. at the `gte_exec` callout from the
  register page. Once recorded vectors exist, exit criterion 1 also
  covers them.
