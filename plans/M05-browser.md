# M5 — Browser host

**Status:** not started
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

1. [ ] `psxw-host-web` skeleton: wasm-bindgen; host interface v1 plus the
   render imports; OPFS disc access; Atomics-based pacing.
2. [ ] `psx-render-gl` running under WebGL2 on an OffscreenCanvas in the
   Worker. Handle context loss: re-create and re-upload VRAM from a shadow
   copy.
3. [ ] `web/` shell: UI, disc import, input polling, AudioWorklet
   playing silence; `psxw build-web`; `psxw serve`.
4. [ ] Dev mode with the separate runtime module; document the hot-swap
   workflow.
5. [ ] Playwright Docker image (pinned) and the cross-host determinism
   test for the `title-to-gameplay` replay, in both browsers.
6. [ ] Performance report: frame-time percentiles in the worker. Reported,
   not gating.
7. [ ] *(optional, if Q1 says yes)* Tauri shell serving the same bundle
   with COOP/COEP.
8. [ ] Add the browser determinism tests to `psxw ci`. Docs, README
   status.

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

## Findings
