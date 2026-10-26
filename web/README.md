# web

The browser host (M5), plain ES modules with no build step:

- `index.html`, `main.js`: the page. It imports a disc (.cue and .bin) into OPFS, polls the keyboard and gamepads into a SharedArrayBuffer, sets up audio (an AudioWorklet playing silence until M6), and starts the Worker.
- `worker.js`: runs `game.wasm` and implements its host interface: pacing with `Atomics.wait`, disc reads from OPFS, pads, saves in OPFS, and the renderer (`pkg/`, built from `crates/psxw-host-web`).
- `disc.js`: .cue/.bin over OPFS sync access handles, with the native host's rules.
- `audio.js`: the AudioWorklet.

To try it:

1. `psxw build-web <game>` assembles `out/web/<game>/`.
2. `psxw serve out/web/<game>` serves it with the cross-origin isolation headers that SharedArrayBuffer needs.
3. Open `http://127.0.0.1:8080/`.

Test hooks (query): `?cue=NAME&autostart&frames=N&verify&hash=60,300&replay=URL`.
