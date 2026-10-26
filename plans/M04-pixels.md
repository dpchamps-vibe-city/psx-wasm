# M4 — Pixels

**Status:** in progress (GPU tasks 3, 5, 6 done; task 4's ABI part done, its native-host imports wait for M2's host)
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

1. [ ] `psx-gte`: register file and all commands, plus unit tests from
   spec examples (debug aids). Wire it into the runtime's private page and
   the intrinsics.
2. [ ] GTE recording hook. Synthetic GTE edge vectors: saturation
   boundaries, division overflow, every command with `sf`/`lm`
   combinations. Run `psxw oracle capture` for GTE, then
   `psxw oracle check --device gte`.
3. [x] GPU front end: packet decoder for every type, state, renderer
   command emission. Unit tests decode every packet type.
4. [ ] Renderer command interface in `psxw-abi`; host imports in the
   native host. (*The interface is done:* `psxw_abi::render`, ABI 2. The
   native host's `render_submit`, `render_readback` and `present` wait
   for M2's host; see Findings, "Integration".)
5. [x] `psx-render-gl` with a headless EGL context (Docker image with Mesa
   llvmpipe, pinned). Implement fill, transfers and copies first, then
   `psxw oracle selftest` against the backend. That gives the verified
   subset.
6. [x] Primitives: sprites and rectangles, then polygons, then lines;
   blending, dither and mask; hazard tracking. Check each feature against
   emulator goldens from synthetic vectors before recorded ones.
7. [ ] libgpu adapters (classification from M1's report) and the libgpu
   conformance harness (register decoder).
8. [ ] libetc adapters (`VSync` modes 0, 1, n, −1; callbacks) and
   conformance.
9. [ ] SIO pad device; libpad and libapi pad adapters; conformance (SIO0
   decoder); replay input path; optional native window input.
10. [ ] GPU recording hook. Author the `title-to-gameplay` replay; record
    vectors; capture goldens; reach zero failures.
11. [ ] `psxw oracle ramdiff btn --replay title-to-gameplay`, with
    `exclude_symbols` covering only sound-driver state (with reasons).
    Reach zero differences.
12. [ ] *(optional)* `asm-gte` normalizer rule; `difftest` on the
    functions that move to C.
13. [ ] `psxw run btn --headless --replay … --dump-frames out/` writes
    PNGs for humans (not gating). Add the GPU and GTE checks to `psxw ci`.
    Docs, README status.

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

- **2026-10-26** (`361b66d`): the GPU tasks' code. `psxw_abi::render`
  (ABI 2); `psx_devices::gpu` (front end); `crates/psx-render-gl`
  (backend over glow, EGL surfaceless, `psxw-render-server`);
  `crates/psxw-gpu-oracle` (the gpu `DeviceUnderTest`, registered in
  `psxw-cli`'s `devices()`); `oracle/docker/render.Dockerfile` and
  `[oracle.render_gl]` in `inputs.lock`. First run of
  `psxw oracle check btn --device gpu`: the 1,050 selftest vectors
  verified, the 3 handwritten GPU vectors provisional, 0 failed.
- **2026-10-26** (`0b91327`): `psxw_oracle::gpu_synth`, seeded generators
  isolating one feature each, installed by `capture` and checked by
  `check`; debugging examples `gpuvec` and `gpucover`.
- **2026-10-26** (`c7ae08d`): self-feedback primitives drawn row by row
  (`prim::SERIAL`), texture reads from texture-coordinate ranges, GP1(01h)
  resets the colour-table cache, six more generators (33 in all), the
  render server's FRAME request and a present test, examples `gpubench`
  and `gpusoak`. Results, from a clean `.oracle/` built in this worktree:
  - `psxw oracle images build`, `psxw oracle selftest`: 1,050 verified.
  - `psxw oracle capture --missing`: 1,320 synthetic GPU vectors
    captured (33 generators × 40, seed `0x505358574d340001`).
  - `psxw oracle check btn --device gpu`: **2,373 vectors: 1,050
    verified (selftest), 1,323 provisional (3 handwritten + 1,320
    synthetic), 0 unresolved, 0 failed.** Every generator: 40 of 40
    provisional. 1,246 of the 1,320 synthetic vectors change their
    readback, 2.55 million pixels in all (`gpucover`), so the vectors
    draw; the rest are legitimately empty (dropped or off-area
    primitives).
  - `psxw oracle capture` (full recapture): all 2,373 GPU vectors
    reproduced byte for byte (one handwritten SPU vector did not; see
    Findings).
  - Soak (`gpusoak`, not archived): seeds `0x50ac01`–`0x50ac04`, 34,650
    vectors (150 and 300 per generator), 0 differ.
  - `cargo test` on the touched crates passes, including
    `psxw-gpu-oracle`'s present test in the pinned image.
- **2026-10-26** (`dac9bb6`): three generators exercising the plumbing
  rather than one feature: `mix` (20–60 primitives of every kind per
  vector, with fills, copies, uploads and state changes between them),
  `clut-many` (more colour-table reloads in one batch than there
  are slots), `batch-big` (more primitives in one batch than one draw
  call holds); example `gpubisect`. Results:
  - `psxw oracle check btn --device gpu`: **2,493 vectors: 1,050
    verified, 1,443 provisional (3 handwritten + 1,440 synthetic from 36
    generators), 0 unresolved, 0 failed.**
  - Soak: seed `0x50ac08` over all 36 generators (5,400 vectors), plus
    400 `mix`, 100 `clut-many` and 100 `batch-big` vectors from seeds
    `0x50ac05`–`07`, and 600 `mix` vectors (with copies and uploads) from
    `0x50ac09`: 0 differ.

## Findings

### Results and throughput (tasks 3, 5, 6)

- **Commands:** `psxw oracle selftest`, `psxw oracle capture --missing`,
  `psxw oracle check btn --device gpu` (counts in the Progress log). The
  gpu device builds its image and the render server on first use; the
  whole check of 2,493 vectors takes about 3–4 s. In all, 41,250 soak
  vectors (not archived) also match.
- **Throughput** (`cargo run --release -p psxw-gpu-oracle --example
  gpubench`, Mesa llvmpipe in Docker, 3 repetitions):
  - backend, per vector including the pipe round trip and VRAM reset:
    0.45 ms (selftest: one fill, copy, upload or rectangle), 1.0 ms
    (synthetic: 1–6 primitives over up to 256×256 pixels);
  - front end, per vector, including compiling the runner ops and
    loading blobs: about 0.24 ms;
  - start-up (image record check, incremental server build, container
    start): about 0.35–0.4 s, plus about 0.1 s of shader compilation on
    the first draws.

  Not optimised yet: a record is 25 texels re-uploaded per draw, the
  fragment shader fetches up to 11 of them, and hazards are tracked per
  bounding box.

### The renderer interface (`psxw_abi::render`, ABI version 2)

- **Encoding:** a stream of `u32` words; each command is
  `op | payload_words << 8` and its payload. Rectangles wrap like VRAM.
  `psxw_abi::render::commands` decodes a stream; `RENDER_VERSION` = 1.
- **Commands:**
  - `SET_STATE`: drawing area and interlace line skip for what follows;
  - `DRAW`: batches of 16-word primitive records (triangle, quad,
    rectangle, line) with explicit attributes;
  - `FILL`, `UPLOAD`, `COPY` (snapshot semantics, mask flags);
  - `READBACK` (offline/server path);
  - `PRESENT` (`PresentDesc`: flags, display area, size);
  - `SYNC` (draw copy → read copy);
  - `CLUT_LOAD` (colour-table cache slots, 1,024 of them).
- **Host imports** (module `host`): `render_submit(ptr, len)`,
  `render_readback(x, y, w, h, dst)` (synchronous, after everything
  submitted), `present(desc_ptr)`. Names in `render::imports`.
- **Who owns what:** the front end owns all PS1 state and all hazard
  policy (where SYNCs go, which reads need them, what is SERIAL, when the
  colour table reloads). The backend is a pure function of commands and
  VRAM.

### Integration (for M2's runtime and host, and task 7)

- **Front end API** (`psx_devices::gpu::Gpu`): `gp0`/`gp0_words`, `gp1`,
  `status` (GPUSTAT), `read` (GPUREAD), `pending_readback` and
  `finish_readback`, `take_commands` (closes the batch; the stream for
  `render_submit`), `flush`, `present_desc`/`emit_present`, `set_field`
  (interlace line skip and GPUSTAT bits 13 and 31), `display`.
- **libgpu mapping:**
  - `DrawOTag`/`DrawPrim`: walk the ordering table in guest RAM and feed
    each node's words to `gp0`;
  - `LoadImage`: `gp0(A0h, xy, wh)` then the pixel words;
  - `MoveImage`: `gp0(80h …)`; `ClearImage`: `gp0(02h …)`;
  - `PutDrawEnv`: the E1–E6 words (and a fill when `isbg`);
  - `PutDispEnv`/`SetDispMask`: GP1(05h–08h)/GP1(03h);
  - `ResetGraph`: GP1(00h)/(01h);
  - `StoreImage`: `gp0(C0h …)`; then `pending_readback()` gives the
    rectangle, `take_commands()` goes to `render_submit`,
    `render_readback` fills guest memory, and `finish_readback(None)`
    ends the transfer (or `Some(pixels)` to serve them through
    GPUREAD);
  - `DrawSync`/`VSync`: `take_commands()` → `render_submit`, and
    `present` with `present_desc()` at each vblank.
- **Native host:** `psx_render_gl::Renderer` implements the three imports
  (`execute`, `read_vram`, present via `PRESENT`/`frame()`). It needs a
  GLES 3.0 context. Headless runs can use the render server in the
  pinned image exactly as the oracle does (`psxw_gpu_oracle::GpuDevice`:
  `reset`, `execute`, `frame`), since Mesa only runs in Docker here.
- **The ABI bump** (1 → 2) means a runtime archive or `game.wasm` built
  earlier must be rebuilt; every user reads the constant.
- **For the GPU recording hook (task 10): packets must hold whole
  commands of at most 16 words.** The device target's linked-list DMA
  checks the GPU's readiness per node only, so a longer command (a long
  polyline, GP0(A0h) with data) that arrives while the GPU is still
  drawing overflows the 16-word FIFO and loses words. The GPU then waits
  forever (seen as a GPU_EXEC device timeout, which also spoils the rest
  of that emulator run). A recorded ordering table with such a node needs
  a runner change (pacing within the node) before it can be a vector.

### How the backend reproduces the GPU

- **Setup on the CPU, pixels in the shader.** `psx_render_gl::setup` does
  Beetle's per-primitive arithmetic:
  - vertex sorting and the "core" vertex;
  - 32.32 edge walkers with their biased start and away-from-zero steps,
    two parts walked up or down;
  - 8.24 interpolant bases and truncated deltas;
  - the line walker's 32.32 steps and colour steps;
  - texture-window masks.

  The fragment shader decides coverage per pixel from those constants,
  with 64-bit edge arithmetic emulated in 32-bit GLSL. It then does the
  interpolation, texel fetch through window and colour table,
  modulation, dither, the four blend modes, and the mask bit, all in
  integers.
- **One instanced draw per batch,** each instance one record drawn as its
  covering rectangle. A quad's two triangles are evaluated in order in
  one fragment invocation, so a pixel both cover is blended twice, as on
  the GPU, without splitting the batch.
- **Self-feedback** (`prim::SERIAL`): a textured primitive whose
  texture-coordinate range can sample where it draws is drawn row by row
  in the GPU's raster order, with a sync after each row. Order:
  rectangles top to bottom; polygon parts in `tripart` order, each up or
  down; quads as two triangles. A soak run found it: big sprites spilling
  into their own page.
- **Colour-table cache:** reloaded only when the table address or depth
  changes, after GP0(01h), GP1(00h) and (psx-spx) GP1(01h). The load
  happens even for primitives that end up dropped. The `clut-cache`
  generator checks a cached table surviving a CPU→VRAM overwrite.
- **Beetle facts the generators confirmed:**
  - lines dither whenever dithering is on (flat ones too); rectangles
    never dither;
  - a horizontally flipped rectangle starts at `u | 1`;
  - a textured polygon's page becomes the draw mode rectangles then use;
  - texture depth 3 draws as 15-bit;
  - fills round x down and width up to 16, wrap, ignore the drawing area
    and mask, and follow the line skip;
  - copies go row by row in 128-pixel chunks; overlapping ones are split
    into snapshot pieces;
  - GP0(C0h) heights above 512 wrap to 9 bits, 0 reads nothing;
  - rows of drawing areas below row 511 plot at `y & 511`;
  - coordinates past ±1024 after the offset wrap to 11 bits (span starts,
    rows, and walking up stops at the wrap);
  - triangles ≥1024 wide or ≥512 tall, and lines as long, are dropped.

### Known gaps (not modelled or not verified)

- **The texture cache is not modelled.** The GPU keeps texels it cached
  even after VRAM under them changes, until GP0(01h), a page change, or a
  CPU↔VRAM or VRAM↔VRAM transfer; Beetle models this. The backend always
  samples current VRAM (D20's read copy).
  - Evidence 1: soak seed `0x50ac01` had 2 of 150 `tex-feedback` vectors
    differ. In both, a page was sampled, then drawn into, then sampled
    again with no invalidation in between, and Beetle served the
    pre-write texels. psx-spx: abusing the cache "is only seen in some
    demos, and never in actual games".
  - Evidence 2: the first `mix` vectors had 1 of 40 differ (`gpubisect`
    found the packet). A big sprite spilled from the destination into a
    page it had sampled. A later sprite sampled the overwritten texels,
    with only a flip-bit change of the draw mode in between, which does
    not invalidate the cache.
  - The generators keep to what the backend models: `tex-feedback`
    always invalidates between the write and the second sampling, and
    `mix` confines drawing to its destination, as a game does to its
    draw buffer. The comments in `gpu_synth.rs` record why.
  - Within-row self-feedback (a row sampling texels it drew itself)
    depends on the cache too, so it is only exact when no cached line
    goes stale.
  - **Proposal for the human:** model the cache only if recorded game
    vectors (task 10) show a game relying on it. Doing it exactly needs a
    sequential fetch simulation per cache epoch.
- **Interlace line skip** (480i, draw-to-display off) is implemented
  (`SET_STATE` skip parity from `set_field`) but unverified: vectors
  carry no GP1 commands, so the device target always draws every row.
- **GP1(01h) resets the colour-table cache** per psx-spx; Beetle does
  not. Unobservable in vectors for the same reason.
- **Beetle's "Wild Arms 2" hack** (`Calc_UVOffsets_Adjust_Verts`)
  rewrites `v` of some 1-pixel-tall triangles with flat `dv/dx`, even in
  the software renderer at 1×. It is an emulator artefact the backend
  does not reproduce. No synthetic or soak vector has hit it; if a
  recorded one does, it fails with this evidence.
- **Presentation** has no device oracle. `tests/present.rs` checks the
  15- and 24-bit conversion against its definition in the pinned image.
- **llvmpipe vs a real GPU** was not compared: the host's GPU would need
  the host's GL libraries, and system-level tools stay in Docker. The
  browser (M5) runs the same integer shaders on real GPUs.

### The `psxw-render-gl` image

- **Pins:**
  - base `debian:trixie-slim@sha256:62bd423a…` and Debian snapshot
    `20261020T213255Z`, both shared with `[oracle]`;
  - Rust 1.94.1, standalone archive sha256 `294b3d81…`, in
    `[oracle.render_gl]`.
- **Contents:** Mesa 25.0.7-2+deb13u1 (llvmpipe, LLVM 19.1.7, "OpenGL ES
  3.2"), libglvnd 1.7.0-1+b2, rustc 1.94.1.
- **Record:** `.oracle/render-gl.json` (pins, image id, sources). The
  image is rebuilt when the pins change or the image is gone; delete the
  record to force a rebuild. The server is built with
  `cargo build --locked` inside the image, from the workspace mounted
  read-only, into `.oracle/render-gl/target`. It runs with
  `--network=none`, one container per check, over a pipe
  (`psx_render_gl::server`).

### Harness notes

- **Docker tags are shared between worktrees.** Another worktree rebuilt
  `psxw-oracle-emu:local` with a changed emulator host (same Beetle core,
  BIOS and runner), which made this worktree's `.oracle/images.json`
  stale. Rebuilding here would have flipped the tag back under the other
  worktree. Instead `images.json` was pointed at the current image, and
  `selftest` reproduced every earlier golden byte for byte. Whoever
  merges should rerun `psxw oracle images build` once.
- **SPU_RUN results depend on what ran before them in the same emulator
  session.** The handwritten vector `spu_noise_voice_3` (`d61a8b7b…`)
  reproduces its golden when captured alone. In a full
  `capture` next to the 1,320 synthetic GPU vectors it always gives a
  different result (709 bytes), so `psxw oracle ci` fails on it here. GPU
  vectors are unaffected (all reproduce). M6 owns SPU_RUN's isolation
  (copied to M06's Context).
- **`gpu_synth::install` replaces** the `synth-gpu-*` sets on every
  `capture`, deleting vectors a changed generator no longer makes, so the
  archive never keeps stale ones.
- **Debugging examples** in `psxw-gpu-oracle`:
  - `gpuvec <id> --diff`: dump a vector and list the pixels that differ;
  - `gpucover`: pixels each generator changes;
  - `gpusoak --seed S --per N`: another seed, any count, captured and
    compared on the spot, keeping only mismatches;
  - `gpubench`: throughput;
  - `gpubisect <id>`: the first packet after which the GPU and the device
    target disagree (captures every packet prefix).
