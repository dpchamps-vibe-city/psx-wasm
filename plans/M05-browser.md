# M5 — Browser host

**Status:** in progress (tasks 1–4, 6 and 8 done; task 5's image and GPU check done, its game check waits for the disc)
**Depends on:** M4. **Unlocks:** M7 (browser saves), a public demo.
**Design refs:** design: Browser host, §6 (blocking), §7 (dev mode);
decisions D16, D18, D20.

## Goal

The same `game.wasm` runs in Chrome and Firefox, inside a Web Worker, with
the WebGL2 backend on an OffscreenCanvas.

The oracle is **cross-host determinism**: for the same replay, the browser
must produce exactly the frame (VRAM) and RAM hashes the native host
produces. Its devices are already gated by M4's oracles.

## Context

- M4 provides the GPU backend in Rust over `glow` (WebGL2 on the web) and
  the renderer command interface, plus a native host that already produces
  per-frame hashes from replays.
- The host interface v1 (M2) plus the render imports (M4) form the whole
  contract the browser host must implement.
- Audio arrives in M6. Build the AudioWorklet path now, playing silence,
  so M6 only fills it.

## Scope

**In**
- **`crates/psxw-host-web`,** a Rust crate compiled to wasm with
  wasm-bindgen. It implements the host interface inside the Worker:
  - pacing with `Atomics.wait` (with timeout) on a SharedArrayBuffer;
  - disc reads through an OPFS `FileSystemSyncAccessHandle`;
  - pad state from a SharedArrayBuffer written by the main thread;
  - `psx-render-gl` over WebGL2 on an OffscreenCanvas;
  - storage through OPFS (used by M7).
- **Wiring:** `game.wasm` is instantiated with its `host.*` imports
  provided by the host module's exports, wired in JavaScript, so calls
  stay wasm to wasm.
- **`web/` (TypeScript):**
  - the main thread: UI (disc import by drag and drop into OPFS; settings
    such as the CD timing toggle and pad mapping; start and pause),
    Gamepad API and keyboard polling into the input buffer, and audio
    context setup;
  - Worker bootstrap;
  - the AudioWorklet consuming a SharedArrayBuffer ring (silence until
    M6).
- **`psxw build-web <game>`** produces `out/web/<game>/`. It is local
  only, since `game.wasm` is game-derived. **`psxw serve`** is a dev
  server with COOP/COEP headers.
- **Dev mode:** a build with the runtime as a separate module, swapped at
  page load; same ABI.
- **A JSPI fallback** for environments without SharedArrayBuffer: a
  documented, best-effort path, tested if the browsers in CI support it.
- **Playwright tests** in a pinned Docker image, running Chromium and
  Firefox headless with WebGL2.

**Optional:** a desktop webview shell (Tauri) if the human chooses it
(Q1).

**Out:** audio content (M6); memory card UI (M7); upscaling.

## Design details

- **Worker loop:**
  1. call `psxw_run_frames(1)`;
  2. inside it, `host.wait_vblank(frame)` blocks with `Atomics.wait`
     until the wall-clock deadline for that frame, computed from the video
     mode's frame period;
  3. if the worker falls behind, it doesn't skip game frames (the game is
     frame-locked); it reports lag instead.
- **Input:** the main thread polls `navigator.getGamepads()` and keyboard
  state once per animation frame. It writes PS1 pad reports into the input
  SharedArrayBuffer using a sequence counter. The worker reads the latest
  report at each vblank. In replay mode, the replay file supplies the
  input instead.
- **Hash stream:** with `?verify=1` (or a test hook), the worker posts the
  per-frame VRAM readback hash and RAM hash. Playwright collects them and
  compares them with the native host's stream for the same replay.

## Tasks

1. [x] `psxw-host-web` skeleton: wasm-bindgen; host interface v1 plus the
   render imports; OPFS disc access; Atomics-based pacing. *(The host
   interface is in `web/worker.js`; `psxw-host-web` is the renderer.)*
2. [x] `psx-render-gl` running under WebGL2 on an OffscreenCanvas in the
   Worker. Handle context loss: re-create and re-upload VRAM from a shadow
   copy. *(Runs and is exact. Context loss: VRAM shadowed every 60
   frames, uploaded into a new renderer on restore; checked in Firefox.)*
3. [x] `web/` shell: UI, disc import, input polling, AudioWorklet
   playing silence; `psxw build-web`; `psxw serve`.
4. [x] Dev mode with the separate runtime module; document the hot-swap
   workflow. *(`?dev`; `web/README.md`.)*
5. [ ] Playwright Docker image (pinned) and the cross-host determinism
   test for the `title-to-gameplay` replay, in both browsers. *(D44:
   `psxw-browsers` instead of Playwright. `psxw web-test gpu` and
   `web-test conform` pass in both browsers. `psxw web-test game` is
   written; it waits for the disc and the replay.)*
6. [x] Performance report: frame-time percentiles in the worker. Reported,
   not gating. *(`frameMs` in the Worker's `hashes` message.)*
7. [ ] *(optional, if Q1 says yes; D46: no for now)* Tauri shell serving the same bundle
   with COOP/COEP.
8. [x] Add the browser determinism tests to `psxw ci`. Docs, README
   status. *(Gates `web-gpu` and `web-game`.)*

## Exit criteria (all automated)

1. `psxw build-web btn` produces a bundle that loads in headless Chromium
   and Firefox and boots from a disc imported into OPFS.
2. Cross-host determinism: for `title-to-gameplay`, every per-frame VRAM
   and RAM hash from each browser equals the native host's. Zero
   mismatches.
3. Pacing: a 60-second idle run keeps the frame count within ±1 frame of
   wall time × frame rate on the CI machine. Reported, and gating only
   where the machine is stated in the report.
4. `psxw ci btn` includes 1–2.

## Risks and decision points

- **Cross-origin isolation** is required for SharedArrayBuffer and
  `Atomics.wait`; `psxw serve` and any host must set COOP and COEP.
  Document it.
- **Gamepad API is main-thread only,** so input latency is up to one
  animation frame. Acceptable; documented.
- **Headless WebGL2** uses software rasterizers (SwiftShader, llvmpipe).
  Integer shaders should still be exact. Any mismatch is a Finding against
  the backend, not something to tolerate.

## Progress log

- **2026-10-26** (`88088da`, `2fda99f`, this commit): the browser host.
  - **`crates/psxw-host-web`:** `WebRenderer`, `psx-render-gl` on
    WebGL2/OffscreenCanvas, through wasm-bindgen (CLI 0.2.129, pinned).
    It draws presented frames with `blit_to_screen` and hashes VRAM as
    the native host does.
  - **`web/worker.js`:** `game.wasm`'s whole host interface. Pacing is
    `Atomics.wait` to each frame's deadline. The disc is read from OPFS
    through sync access handles (`web/disc.js` follows the native cue
    rules). Pads come from a SharedArrayBuffer or a replay; saves are kept
    in OPFS. Verify mode collects RAM (`psxw_ram_hash`) and VRAM hashes.
  - **The page:** `web/main.js` imports a disc and polls the keyboard and
    gamepads. Test hooks (`?discurl`, `?post`) let a headless browser
    import a disc and report.
  - **Commands:** `psxw build-web <game>`, and `psxw serve` (COOP/COEP).
  - **`psxw web-test gpu`** (D44: Debian-snapshot Chromium and Firefox
    ESR, `psxw-browsers`, 1.1 GB):
    - Chromium on SwiftShader: **2,493 of 2,493 GPU vectors equal their
      goldens**, in 12.0 s;
    - Firefox on llvmpipe: **2,493 of 2,493**, in 2.2 s.

    Headless Firefox finds no GL driver ("Exhausted GL driver options"),
    so it runs under Xvfb, where GLX over llvmpipe works.
- **2026-10-26** (this commit): tasks 2, 4 and 6.
  - **Context loss:**
    - The Worker shadows VRAM every 60 frames. While the context is
      lost, it skips submit, readback and present.
    - On `webglcontextrestored` it builds a new `WebRenderer` and calls
      `upload_vram` (one `UPLOAD` of 1024×512).
    - `psxw web-test gpu` now loses and restores the context with the
      largest vector's VRAM in it. The new renderer must hash the same,
      and does in Firefox.
    - Headless Chromium on SwiftShader fires `webglcontextlost` but never
      `webglcontextrestored`. A probe found the same for a plain canvas,
      an OffscreenCanvas and a transferred one in a Worker, so the check
      is reported as not exercised there.
  - **Dev mode (`?dev`):**
    - The Worker instantiates guest.wasm and then runtime.wasm, wired
      through JS trampolines.
    - The guest now carries the adapter table it was linked against
      (`psxw.adapters`). Dev mode refuses a runtime with another table.
    - `psxw build-web` copies both modules and checks the wiring (with
      wasmparser): the guest's imports, the runtime's imports, the
      table, and that the pair exports everything game.wasm exports.
    - A Node mirror of `instantiateDev` against btn's bundle resolved
      every import, with ABI 3.
  - **Frame times:** p50, p95, p99 and max of the wall time between
    vblanks, posted with the hashes.

- **2026-10-26** (this commit): `psxw web-test conform <game>` (gate
  `web-conform`), a disc-free cross-host determinism check.
  - **How it works:** every conformance scenario (libpad, libgpu,
    libmcrd, libetc) runs natively on `game.wasm` with the server
    renderer, and every step is recorded with its result. The page
    replays the steps on the same `game.wasm` in each browser, with the
    WebRenderer and a host like the native conformance one.
  - **Result:** all 1,334 steps match in Chromium and Firefox, every
    return value and read, and so do the final VRAM hashes and the
    audio frames and hash. That covers libetc's interrupt exit through
    wasm exceptions (D45) in both browsers.
  - **The check works:** a tampered return value in the recording is
    reported (`got 0x8005650c, want 0x12345`).

## Findings

### From M4's GPU backend (2026-10-26)

- **`psx-render-gl` needs only WebGL2 core, no extensions.** It uses:
  - an `R16UI` 1024×512 colour target plus an `R16UI` read copy, blitted
    (`NEAREST`) between framebuffers;
  - an `RGBA32UI` 1024×64 texture holding the primitive setup records;
  - `texelFetch` on `usampler2D` in both stages, and an integer
    fragment output;
  - `clearBufferuiv`;
  - `gl_VertexID`/`gl_InstanceID` with no vertex attributes (a bound
    VAO);
  - shaders in GLSL ES 3.00, integer arithmetic only, with 64-bit edge
    arithmetic emulated in 32 bits.
- **Check early on real browsers:**
  - `int(uint)` must keep the bit pattern (Mesa does; ANGLE's D3D and
    Metal back ends should be confirmed by the cross-host determinism
    check);
  - shader compile time and per-call validation overhead. The fragment
    shader fetches up to 11 setup texels per pixel, and self-feedback
    primitives draw one row per call.
- **Readback:** `readPixels` uses `RED_INTEGER`/`UNSIGNED_SHORT` when the
  implementation offers it for `R16UI`, else `RGBA_INTEGER`/
  `UNSIGNED_INT` (16 bytes a pixel). `render_readback` must be
  synchronous, which `readPixels` on an OffscreenCanvas context in the
  worker is.
- **Present:** `PRESENT` renders the display area (15- or 24-bit) into an
  RGBA8 texture whose row 0 is the picture's bottom row, ready to blit
  to a default framebuffer. The final blit to the canvas is still to
  write (`Renderer::gl()` exposes the context). `PresentDesc` gives the
  visible width and height from GP1(06h)–(08h).
- **Imports to implement:**
  - `render_submit(ptr, len)`: `Renderer::execute` on the words;
  - `render_readback(x, y, w, h, dst)`: `Renderer::read_vram`;
  - `present(desc_ptr)`: a `PresentDesc` of 5 `u32`s.

  Names are in `psxw_abi::render::imports`. M4's Findings,
  "Integration", has the front end side.
