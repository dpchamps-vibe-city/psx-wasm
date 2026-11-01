# Implementing a host

This guide describes the **existing game/host interface**, its browser and native
implementations, and the separate host-internal visual-renderer interface. It is
not a specification for a new renderer or geometry-capture protocol.

The contract's source of truth is [`psxw-abi`](../crates/psxw-abi/src/lib.rs):

- `host`: the general host imports and their signatures.
- `exports`: the entry points exposed by `game.wasm`.
- `BootOptions`, `DiscInfo`: shared memory layouts.
- [`render`](../crates/psxw-abi/src/render.rs): rendering imports, command encoding,
  primitive records, display descriptors and synchronization semantics.
- [`replay`](../crates/psxw-abi/src/replay.rs): recorded input format and buttons.

The current `ABI_VERSION` is **12** and `render::RENDER_VERSION` is **1**. Consult
those definitions when updating a host; do not infer compatibility from this
snapshot or from an older build's documentation.

## Boundaries and terminology

In a normal build, the compiled game and SDK are merged with the Rust runtime
into `game.wasm`. The runtime includes adaptors, device support, the fixed-point
GTE, and the GPU front end. These Rust components execute **inside Wasm**; calling
them native backends does not place them in a native OS process.

```text
GAME EXECUTION — game.wasm
  compiled game + SDK
          │
  adaptors / original drivers with device fallback
          │
  shared GPU core/front end (psx-devices::gpu)
  packet decoding, GPU state, batching and VRAM hazards
          │
========== GAME WASM ↔ HOST ABI ============================
  host.render_submit / host.render_readback / host.present
          │
  browser Worker or native Wasmtime host
          │
  host rendering implementation
```

The other host services—disc access, pad input, storage, audio, logging and
pacing—cross the same boundary. The host supplies platform services, not an
emulated CPU, SDK driver implementation or replacement GTE.

Dev mode can keep `guest.wasm` and `runtime.wasm` separate, sharing memory and
wired through host glue. That adds a guest/runtime seam without moving runtime
responsibilities into the host. See [design §7](design.md#7-one-module-with-a-prebuilt-runtime-merged-in)
and [the emulation layers](emulation-layers.md).

### Existing host implementations

| | Browser | Native tests/CLI |
|---|---|---|
| Host import implementation | [`web/worker.js`](../web/worker.js) | [`psxw-host-native/src/lib.rs`](../crates/psxw-host-native/src/lib.rs) |
| Game execution | WebAssembly in a Worker | Wasmtime running the game Wasm |
| Disc/storage | OPFS-backed disc and card files | Disc images and storage directory |
| Audio | Shared ring to an AudioWorklet | Audio sink for hashing/optional WAV output |
| Rendering bridge | [`WebRenderer`](../crates/psxw-host-web/src/lib.rs) | [`Render`](../crates/psxw-host-native/src/render.rs) trait |
| Real rendering backend | `psx-render-gl` compiled to Wasm, using WebGL2 | `psx-render-gl` compiled natively, using EGL/GLES in the render server |
| Frame destination | OffscreenCanvas → ImageBitmap → page | RGBA frames for tests and dumps |

The browser renderer is a **separate Wasm module**,
`pkg/psxw_host_web_bg.wasm`, called by the Worker through wasm-bindgen. It is a
host-service implementation, not part of `game.wasm`. It has its own memory and
runs in the same Worker as the game. WebGL calls go through browser bindings;
pixel rendering is GPU shader work.

That packaging lets the browser and native backend share Rust rendering code
and lets renderer updates avoid rebuilding game binaries. It is not required by
WebGL or by the game/host contract: a JavaScript renderer could implement the
same service. The split adds calls and command-buffer copying, not automatic
parallelism or a guaranteed performance advantage.

Native runs default to `NullRender`, which performs no drawing and returns zero
pixels on readback. `psxw run <game> --render server` selects
[`ServerRender`](../crates/psxw-cli/src/render.rs), which forwards commands over
pipes to the native render-server executable in the pinned container. This is a
headless EGL/GLES path, not a desktop window. Its framing is defined in
[`psx-render-gl/src/server.rs`](../crates/psx-render-gl/src/server.rs).

## ABI and memory conventions

- Imports belong to the Wasm module named **`host`**, not WASI. Supply every
  import declared by the game module, including hooks for services the host
  chooses not to use.
- Pointers are **unsigned 32-bit offsets in the game's linear memory**, not
  host pointers, original PS1 addresses, or offsets in the browser renderer's
  separate memory. Shared values are little-endian.
- Import scalar arguments are Wasm `i32` unless noted. Interpret pointer/count
  arguments as unsigned; browser code uses `>>> 0` for pointers above 2 GiB.
  `i64` arguments/results use JavaScript `BigInt` at the JS/Wasm boundary.
- Strings use explicit byte lengths. Refresh memory views after calls that can
  grow memory, and copy input data if retaining it beyond the import call.
- To inspect PS1 memory, read `guest_window()` (falling back to `WINDOW_BASE` as
  the existing hosts do). Do not hardcode the normal window for debug builds.
  Runtime import pointers are already Wasm offsets: do **not** translate them
  through the guest window again.
- The module uses Wasm exception handling for interrupt exits. A native engine
  must enable the required support; see the existing Wasmtime configuration.

`ABI_VERSION` covers the host interface, guest/runtime seam and boot options.
The hosts compare `psxw_abi_version()` with the version they implement and reject
a mismatch. The rendering command version is part of that overall contract.

## Startup and lifecycle

A minimal normal-build host follows this sequence:

1. Provide the module's `host.*` imports and instantiate `game.wasm`.
2. Check `psxw_abi_version()` before booting; obtain exported `memory` and the
   guest window.
3. Call `psxw_alloc(24)` for `BootOptions`. Reacquire the memory view and write
   six little-endian `u32`s in this order: `cd_timing`, `video`,
   `safepoint_budget`, `safepoint_quantum`, `debug`, `trace`. Use the field and
   flag definitions in `psxw-abi`; zero-initialized options are supported by the
   existing browser startup.
4. Call `psxw_boot(options_ptr)`. The game's loop drives execution and invokes
   host services. Boot is not a function the host calls once per frame.
5. Handle normal return or a host/runtime trap, collect reports and release
   platform resources. The existing hosts also use a trap to enforce a frame
   limit; a trap is not necessarily a game fault.

Browser JSPI support wraps `wait_vblank` with `WebAssembly.Suspending` and calls
boot through `WebAssembly.promising`. It is not required to instantiate and run
the game, but provides Worker event-loop turns needed for responsive browser
operation. The existing dev-mode JS trampolines do not support this suspension
path. See [the browser notes](../web/README.md).

Additional exports serve specific host tools:

| Exports | Purpose |
|---|---|
| `psxw_ram_hash`, `psxw_spu_hash` | State checks for replays and conformance. |
| `psxw_conform_init`, `psxw_call`, `psxw_run_vblanks` | Disc-free, call-by-call conformance execution. |
| `psxw_execute` | Queue a guest call; completion arrives as an `execute` event through `host.report`. |
| `psxw_js_enter`, `psxw_js_leave` | Bracket script-mod JS calls so the runtime can hold pacing waits while JS is on the stack. |

Use the documented safe entry points rather than re-entering arbitrary game
functions from a host callback. In particular, `psxw_execute` queues execution
at a runtime-controlled boundary; it does not execute the requested function
synchronously inside `vblank`.

## General host imports

Signatures below omit the `host.` prefix. Unless shown otherwise, all parameters
and status/count results are Wasm `i32`; `()` means no result.

| Import | Required behavior |
|---|---|
| `log(level, ptr, len) → ()` | Consume a length-delimited log message. |
| `trap(code, ptr, len) → ()` | Terminate the current run with the diagnostic. Do not return normally. |
| `vblank(frame: i64) → ()` | Apply frame-boundary work visible to the game: input/replay frame state, mod swaps, queued operations and checks. **Never suspend.** |
| `wait_vblank(frame: i64) → ()` | Pace/yield only; do not change game-visible state or advance virtual time. Native tests use a no-op. |
| `disc_info(out) → i32` | Write `DiscInfo`; return 0 for no disc, nonzero when present. |
| `disc_read(lba, count, mode, dst) → i32` | Read sectors synchronously: mode 0 = raw 2352-byte sectors, mode 1 = 2048-byte user data. Return 0 on success, negative on failure. |
| `pad_state(port, out) → i32` | Return bytes written, or 0 for disconnected. Current hosts write a two-byte little-endian active-low digital button mask. |
| `storage_read(name_ptr, name_len, dst, cap) → i32` | Read a named file, limited to `cap`; return bytes read or -1 on failure. |
| `storage_write(name_ptr, name_len, src, len) → i32` | Accept a named file's bytes; return status (0 on success in existing hosts). Browser persistence is queued to the page. |
| `report(ptr, len) → ()` | Consume a length-delimited JSON run-report event, including boot, exit and queued-call completion events. |
| `audio(ptr, frames) → ()` | Consume 44,100 Hz stereo PCM: `frames` interleaved little-endian `i16` pairs, four bytes per frame. |

The runtime owns virtual time, interrupt delivery, device timing, audio mixing
and pad protocol behavior. For example, `audio` supplies already-mixed samples;
the host does not mix PS1 voices. The runtime supplies the number of samples due
in virtual time rather than asking the host to derive it from wall-clock time.

The separation between `vblank` and `wait_vblank` is semantic, not just a browser
optimization. The runtime may defer pacing waits while script JS is on the
stack; game-visible frame work must not move into those waits.

## Rendering imports and responsibilities

| Import | Required behavior |
|---|---|
| `render_submit(ptr, len) → ()` | Execute the encoded commands in order. **`len` is bytes**, a multiple of four, not a word or primitive count. |
| `render_readback(x, y, w, h, dst) → ()` | Synchronously write `w*h` little-endian `u16` canonical VRAM pixels, row-major, after preceding submissions. Rectangles wrap at VRAM boundaries. |
| `present(desc_ptr) → ()` | Present the display described by five `u32`s: flags, x, y, width, height. This is `PresentDesc`, not a pointer to RGBA pixels. |

The GPU front end in the runtime has already decoded PS1 packets into explicit
renderer records. The host backend executes draw batches, uploads, fills,
copies, synchronization and colour-table cache loads. Do not replace this with
an ordinary textured-triangle API that loses PS1 masks, blending, drawing order,
VRAM feedback or cache semantics.

The full word encoding and per-operation rules remain in
[`render.rs`](../crates/psxw-abi/src/render.rs), rather than being duplicated
here. In particular, canonical draw/read VRAM copies and `SYNC` preserve the
specified visibility of prior writes. Game-visible reads must use that
canonical state, not an enhanced display surface.

### The separate visual-renderer interface

Inside the existing `psx-render-gl` implementation:

```text
renderer commands
       │
canonical GPU execution ───────────────→ game-visible VRAM/readbacks
       │ decoded commands + read-only canonical VRAM/CLUT
       ▼
selected VisualRenderer
  Original:       present canonical VRAM
  Clean Polygons: redraw into independent enhanced visual surfaces
  custom:         implementation-owned visual rendering
       │
       ▼
RGBA output → browser canvas or native frame readout
```

Canonical execution runs regardless of the visual selection. Visual surfaces
must never overwrite canonical VRAM or supply game readbacks. Renderer choice,
internal resolution, filtering and colour options are host-local settings, not
fields in the game/host command ABI.

[`VisualRenderer`](../crates/psx-render-gl/src/visual.rs) is a Rust
**host-internal WebGL2/GLES interface**, not another `host.*` import contract or a
cross-graphics-API plugin system. See the
[host renderer guide](../crates/psx-render-gl/README.md) for its factory,
callbacks, GL-state rules and context-restoration obligations.

The browser exposes built-in visual styles through its Rendering panel. The
native library can also use these implementations, but the current native
`Render`/render-server protocol does not expose visual-style selection.

## Recording hooks

These imports must still be supplied when recording is disabled. Existing
browser implementations decline recording with 0 and ignore the data hooks;
native tools implement them for oracle capture.

| Import | Behavior |
|---|---|
| `gpu_record_begin() → i32` | Offer a GPU linked-list batch for capture; 0 declines. Nonzero accepts, with preceding rendering submitted so the host can capture initial VRAM. |
| `gpu_record(ptr, len) → ()` | Consume the accepted batch's initial GPU state words and packet nodes. Encoding is in `render::imports`. |
| `spu_record_begin() → i32` | Request a recording window's sample count, or return 0 to decline. |
| `spu_record(ptr, len) → ()` | Consume a completed sample/register/RAM recording. Encoding is in `host::SPU_RECORD` documentation. |

The runtime offers these hooks when the corresponding `BootOptions::trace`
flags are set. They record existing GPU packets or SPU state; they are **not a
3D geometry metadata channel**.

## Implementing or changing a host

Use the existing hosts as executable references, not just this summary:

- [`psx-runtime/src/wasm.rs`](../crates/psx-runtime/src/wasm.rs): actual import
  declarations, export implementations and the runtime's host adapter.
- [`web/worker.js`](../web/worker.js) and [browser guide](../web/README.md):
  startup, memory access, synchronous I/O, JSPI, presentation and context loss.
- [`psxw-host-native/src/lib.rs`](../crates/psxw-host-native/src/lib.rs):
  Wasmtime configuration, import wiring, boot and report collection.
- [`psxw-host-native/src/render.rs`](../crates/psxw-host-native/src/render.rs),
  [`psxw-cli/src/render.rs`](../crates/psxw-cli/src/render.rs) and
  [`psxw-gpu-oracle`](../crates/psxw-gpu-oracle/src/lib.rs): the native renderer
  interface, bridge and server transport.

Start with one game/replay and compare the relevant RAM, canonical VRAM and audio
hashes with an existing host. Exercise synchronous readback and the separation
of frame work from pacing. Reuse the existing renderer integration tests and
browser GPU suite when changing rendering; do not invent a second conformance
framework for the host.

A contract change requires coordinated ABI definitions, runtime calls and host
implementations. An unused new Wasm import is not automatically optional at
instantiation; compatibility must be handled explicitly.

### Not part of the current interface

There is no per-vertex 3D/depth feed or ray-based visual renderer in this
contract. Current primitive records contain screen-space XY, colours and UVs.
A future geometry supplement would require reliable association with submitted
primitives, a defined coordinate space/lifetime, and an explicit data path across
the Wasm/host boundary and relevant host transports. GTE observation alone does
not establish that association. This guide does not prescribe or reserve a wire
format for that proposal. The proposed addon-based approach and its feasibility
and compatibility gates are tracked in the
[geometry-provider addon plan](../plans/geometry-provider-addons.md).
