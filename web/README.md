# web

The browser host (M5), plain ES modules with no build step:

- `index.html`, `main.js`: the page. It imports a disc (.cue and .bin) into OPFS, polls the keyboard and gamepads into a SharedArrayBuffer, sets up audio (an AudioWorklet playing silence until M6), and starts the Worker.
- `worker.js`: runs `game.wasm` and implements its host interface: pacing with `Atomics.wait`, disc reads from OPFS, pads, saves in OPFS, and the renderer (`pkg/`, built from `crates/psxw-host-web`).
- `disc.js`: .cue/.bin over OPFS sync access handles, with the native host's rules.
- `cards.js`: memory cards in OPFS `saves/` (`card0.mcr`, `card1.mcr`), imported and exported as raw 128 KiB images from the page (before Start), loaded and written back by the Worker.
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

## Dev mode

`?dev` runs the unmerged `guest.wasm` and `runtime.wasm` instead of `game.wasm`. The Worker instantiates the guest first (it defines the memory), with its `psx.*` imports forwarding to the runtime's exports, then the runtime with the guest's exports as `guest.*`. Every guest call into the runtime crosses JS, so it is slower than the merged module.

It lets a runtime change be tried without relinking the game:

1. Change the runtime (`crates/psx-runtime`, `psx-bios`, `psx-devices`, ...).
2. `psxw runtime build` writes `out/runtime/runtime.wasm`.
3. Copy it over `out/web/<game>/runtime.wasm` and reload the page with `?dev`.

The guest calls adapters by id, so it records the adapter table it was linked against (`psxw.adapters`). If the new runtime's table differs (an adapter added, removed or renamed), the Worker refuses to start; run `psxw build-web <game>` to relink. `psxw build-web` also checks that the pair wires up: imports, exports and the table.

## WebGL context loss

The Worker keeps a copy of VRAM, taken every 60 frames. When the context is lost it stops drawing and reading back; the game runs on. When the context is restored it makes a new renderer and uploads the copy, so up to 60 frames of drawing are missing until the game redraws them. `psxw web-test gpu` loses and restores the context with `WEBGL_lose_context` and checks the restored VRAM hash. Headless Chromium on SwiftShader never restores a lost context, for any canvas kind, so the check runs in Firefox only.
