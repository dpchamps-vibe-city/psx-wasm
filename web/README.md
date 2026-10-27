# web

The browser host (M5), plain ES modules with no build step:

- `index.html`, `main.js`: the page. It imports a disc (.cue and .bin) into OPFS, polls the keyboard and gamepads into a SharedArrayBuffer, sets up audio (an AudioWorklet playing silence until M6), and starts the Worker.
- `worker.js`: runs `game.wasm` and implements its host interface: pacing with `Atomics.wait`, disc reads from OPFS, pads, saves, and the renderer (`pkg/`, built from `crates/psxw-host-web`). Where the browser has JSPI (`WebAssembly.Suspending`), the game suspends once a vblank and gives the Worker's event loop a turn. Chromium frees the frames the Worker hands the page only there: without it the tab's memory grows by a frame a vblank (gigabytes a minute) until the GPU process dies and the screen goes black. Dev mode can't suspend (its JS trampolines sit between the game and `wait_vblank`) and warns in the console.
- `disc.js`: .cue/.bin over OPFS sync access handles, with the native host's rules.
- `cards.js`: memory cards in OPFS `saves/` (`card0.mcr`, `card1.mcr`), imported as raw 128 KiB images from the page before Start, and exported any time (during a run, the game's latest card). The Worker loads them at start and posts each card the game changes to the page, which writes it (`SaveQueue`).
- `audio.js`: the AudioWorklet.

To try it:

1. `psxw build-web <game>` assembles `out/web/<game>/`.
2. `psxw serve out/web/<game>` serves it with the cross-origin isolation headers that SharedArrayBuffer needs.
3. Open `http://127.0.0.1:8080/`.

Test hooks (query): `?cue=NAME&autostart&frames=N&verify&hash=60,300&replay=URL&record`.

At the end of a run the Worker posts frame times (the wall time between vblanks, after pacing): `frameMs` with p50, p95, p99 and max, next to `lagFrames` (frames more than a period late). They are reported, not gated.

## Recording a replay

Tick **Record a replay** before Start, play, then press **Save replay** (any time; the recording also goes out when the run stops). The download is a psxw-abi replay (version 2): its entries are indexed by the game's frame counter, the manifest's `[oracle] frame_counter`, which `psxw build-web` writes to `game.json`. Entry `c` holds the pads the game saw at the last vblank its counter read `c`, so the native host and the emulator replay the session at the same game frames, whatever their load times.

Keyboard: arrows, Enter = Start, Right Shift = Select, Z = Square, S = Cross, X = Circle, W = Triangle, A/D = L1/R1, Q/E = L2/R2; or a gamepad.

Put the file at `games/<game>/replays/<name>.rpl`; then `--replay <name>` finds it for `psxw run`, `psxw oracle play|ramdiff|record` and `psxw web-test game`. `psxw web-test game <game> --replay R --record` checks the recorder: the page records while it plays R, and the recording, played natively, must reach the same RAM.

Cards persist in this browser's OPFS between runs, so a session that saves can be followed by one that loads; export the card to keep it.

## One-shot console scripting

After Start and the first vblank, the browser console can call an existing
compiled game function:

```js
// Set address to the guest address printed by:
// psxw mod show <game> <symbol>
const result = await window.wasmPsxScripting.execute(address, 100);
const v0 = Number(result & 0xffffffffn);
```

`execute(address, ...args)` accepts up to four unsigned 32-bit o32 argument
words and returns a Promise of the raw `v0 | (v1 << 32)` BigInt. It queues
one call, runs it once outside runtime borrows, interrupt handlers and
critical sections, and returns its result. Only one call may be pending.
It requires JSPI and the merged module (not `?dev`); rebuild the bundle for
ABI 8. Errors before readiness, while busy, or after stop reject the Promise.

A mod loaded into a moddable bundle (`psxw mod watch`, the mod panel) can
also be called by the name of one of its global functions, such as
`execute("RotateWorld", 1)`. The console logs each one's synthetic address
when mods load or swap. `psxw run --execute FRAME:FUNC[:ARG,..]` does the
same headless.

This is an unsafe developer tool, not a sandbox: choose a valid function,
the right arguments and a resident overlay. A bad call can trap or hang the
game or corrupt its state; there is no rollback. It calls code already in
`game.wasm`, not newly injected JS or wasm. Symbol lookup already exists in
the build; source maps are for debugging and are not needed for this API.
There are no persistent hooks or game-state wrappers yet.

## Debugging the C in DevTools

A debug bundle carries DWARF for every C function, decompiled or lifted (D53):

```sh
psxw build-web <game> --debug
psxw serve out/web/<game>-debug
```

Chrome reads DWARF through the **C/C++ DevTools Support (DWARF)** extension. Without it, DevTools shows only the wasm. Open the page and start the game. DevTools → Sources then lists the C files:
- the decomp's `src/` and `include/`, at their paths on the build machine;
- `out/lift/…/<address>.c` for translated functions. These are generated C, one MIPS instruction a line, with the original instruction in a comment.

Set a breakpoint on a C line. When it hits, the Scope pane shows the C locals and parameters with their types, and pointers expand into the game's structs. The Console evaluates C expressions.

- **Stepping is uneven.** The C is compiled at `-O2`: stepping jumps between lines, a breakpoint may land on the nearest line with code, and some variables show as optimized out.
- **The extension may need file access.** If DevTools can't load a source, turn on "Allow access to file URLs" on the extension's page in `chrome://extensions`.
- **Not every pointer reads right.** The guest's window is at `0x8000_0000`, so the game's KSEG0 pointers (`0x80xx_xxxx`) are wasm addresses the debugger reads directly. Scratchpad (`0x1F80_xxxx`) and mirror pointers (`0x00xx_xxxx`, `0xA0xx_xxxx`) show zeros. The module declares about 2 GiB of memory to put the window there; the browser doesn't commit the unused part.
- **No mods yet.** Debug bundles don't take mods.

## Dev mode

`?dev` runs the unmerged `guest.wasm` and `runtime.wasm` instead of `game.wasm`. The Worker instantiates the guest first (it defines the memory), with its `psx.*` imports forwarding to the runtime's exports, then the runtime with the guest's exports as `guest.*`. Every guest call into the runtime crosses JS, so it is slower than the merged module.

It lets a runtime change be tried without relinking the game:

1. Change the runtime (`crates/psx-runtime`, `psx-bios`, `psx-devices`, ...).
2. `psxw runtime build` writes `out/runtime/runtime.wasm`.
3. Copy it over `out/web/<game>/runtime.wasm` and reload the page with `?dev`.

The guest calls adapters by id, so it records the adapter table it was linked against (`psxw.adapters`). If the new runtime's table differs (an adapter added, removed or renamed), the Worker refuses to start; run `psxw build-web <game>` to relink. `psxw build-web` also checks that the pair wires up: imports, exports and the table.

## WebGL context loss

The Worker keeps a copy of VRAM, taken every 60 frames. When the context is lost it stops drawing and reading back; the game runs on. When the context is restored it makes a new renderer and uploads the copy (both events arrive only where the game yields, with JSPI), so up to 60 frames of drawing are missing until the game redraws them. `psxw web-test gpu` loses and restores the context with `WEBGL_lose_context` and checks the restored VRAM hash. Headless Chromium on SwiftShader never restores a lost context, for any canvas kind, so the check runs in Firefox only.
