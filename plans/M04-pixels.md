# M4 — Pixels

**Status:** not started
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

## Findings
