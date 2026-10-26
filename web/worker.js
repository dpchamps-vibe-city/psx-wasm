// The browser host (M5): runs game.wasm in this Worker and implements its
// host interface (crates/psxw-abi: `host.*`, plus the M4 render imports):
// - pacing: `wait_vblank` sleeps with Atomics.wait until the frame's
//   wall-clock deadline (no pacing in verify mode);
// - the disc: a .cue/.bin image in OPFS (`disc/`), sync access handles;
// - pads: the main thread's SharedArrayBuffer, or a replay file;
// - storage (memory cards): files in OPFS `saves/` (cards.js), loaded at
//   start and written back as the game saves;
// - rendering: psxw-host-web's WebRenderer (psx-render-gl on WebGL2) on
//   the OffscreenCanvas.
//
// - dev mode (`options.dev`): guest.wasm and runtime.wasm instantiated
//   separately and wired through JS trampolines, so a rebuilt runtime is
//   picked up without relinking the game (slower: every runtime call
//   crosses JS);
// - WebGL context loss: VRAM is shadowed every 60 frames and uploaded
//   into the re-created renderer when the context is restored (the
//   colour-table cache refills on the next table change).
//
// Messages in: {type: "start", canvas, cue, padBuffer, options}.
// Messages out: {type: "status"|"report"|"log"|"hashes"|"stopped", ...}.

import init, { WebRenderer } from "./pkg/psxw_host_web.js";
import { Disc, SECTOR, userData } from "./disc.js";
import { loadSaves, persist, savesDir } from "./cards.js";

const ABI_VERSION = 4;
const NOTHING_PRESSED = 0xffff;
const REPLAY_HEADER = 56;
const FRAME_LIMIT = "psxw: frame limit";

onmessage = async (e) => {
  if (e.data.type !== "start") return;
  try {
    await run(e.data);
  } catch (err) {
    postMessage({ type: "stopped", reason: String((err && err.message) || err) });
  }
};

/** Pad buttons for `port` at `frame` from a replay (psxw-abi replay). */
function replayButtons(replay, frame, port) {
  const dv = new DataView(replay.buffer, replay.byteOffset, replay.byteLength);
  const frames = dv.getUint32(16, true);
  if (frame >= frames) return NOTHING_PRESSED;
  return dv.getUint16(REPLAY_HEADER + 4 * frame + 2 * port, true);
}

/**
 * Dev mode: guest.wasm defines the memory and imports `psx.*` from the
 * runtime; runtime.wasm imports `guest.*` (the guest's exports, memory
 * included) and `host.*`. The guest goes first, its `psx.*` imports
 * forwarding to the runtime once it exists.
 */
async function instantiateDev(host) {
  const [guestModule, runtimeModule] = await Promise.all(
    ["guest.wasm", "runtime.wasm"].map((f) => WebAssembly.compileStreaming(fetch(new URL(`./${f}`, import.meta.url)))),
  );
  // The guest calls adapters by id: a runtime with another table needs
  // the game relinked.
  const table = (m) => new TextDecoder().decode(WebAssembly.Module.customSections(m, "psxw.adapters")[0] || new ArrayBuffer(0));
  if (table(guestModule) !== table(runtimeModule)) {
    throw new Error("runtime.wasm's adapters differ from those guest.wasm was linked against: rebuild with `psxw build-web`");
  }
  let runtime = null;
  const psx = {};
  for (const imp of WebAssembly.Module.imports(guestModule)) {
    if (imp.module === "psx") psx[imp.name] = (...args) => runtime.exports[imp.name](...args);
  }
  const guest = await WebAssembly.instantiate(guestModule, { psx });
  const guestImports = {};
  for (const imp of WebAssembly.Module.imports(runtimeModule)) {
    if (imp.module === "guest") guestImports[imp.name] = guest.exports[imp.name];
  }
  runtime = await WebAssembly.instantiate(runtimeModule, { guest: guestImports, host });
  return { ...guest.exports, ...runtime.exports, memory: guest.exports.memory };
}

async function run({ canvas, cue, padBuffer, options }) {
  await init({ module_or_path: new URL("./pkg/psxw_host_web_bg.wasm", import.meta.url) });
  let renderer = new WebRenderer(canvas);
  let lost = false;
  let shadow = null;
  canvas.addEventListener("webglcontextlost", (e) => {
    e.preventDefault();
    lost = true;
    postMessage({ type: "status", text: "WebGL context lost; waiting for it to come back" });
  });
  canvas.addEventListener("webglcontextrestored", () => {
    renderer = new WebRenderer(canvas);
    if (shadow) renderer.upload_vram(shadow);
    lost = false;
    postMessage({ type: "status", text: "WebGL context restored" });
  });
  postMessage({ type: "status", text: `renderer: ${renderer.info()}` });

  const root = await navigator.storage.getDirectory();
  const disc = await Disc.open(await root.getDirectoryHandle("disc"), cue);
  const saveDir = await savesDir();
  const saves = await loadSaves(saveDir);
  const replay = options.replay ? new Uint8Array(await (await fetch(options.replay)).arrayBuffer()) : null;
  const pad = new Int32Array(padBuffer); // [seq, port 1 buttons, port 2 buttons or -1]
  const sleeper = new Int32Array(new SharedArrayBuffer(4));
  const period = options.pal ? 1000 / 49.761 : 1000 / 59.826;
  const hashAt = new Set(options.hashAt || []);
  const ramHashes = {};
  const vramHashes = {};
  let lagFrames = 0;
  const frameTimes = [];
  let lastVblank = 0;

  let exports = null;
  const mem = () => exports.memory.buffer;
  const bytes = (p, n) => new Uint8Array(mem(), p, n);
  const text = (p, n) => new TextDecoder().decode(bytes(p, n).slice());
  let frame = 0;
  let t0 = 0;
  const raw = new Uint8Array(SECTOR);

  const host = {
    log(level, p, n) {
      postMessage({ type: "log", level, frame, text: text(p, n) });
    },
    trap(code, p, n) {
      throw new Error(`runtime trap ${code}: ${text(p, n)}`);
    },
    wait_vblank(f) {
      frame = Number(f);
      const now = performance.now();
      if (lastVblank) frameTimes.push(now - lastVblank);
      if (frame % 60 === 0 && !lost) shadow = renderer.vram();
      if (hashAt.has(frame)) {
        ramHashes[frame] = (exports.psxw_ram_hash() & 0xffffffffffffffffn).toString(16).padStart(16, "0");
        vramHashes[frame] = renderer.vram_hash();
      }
      if (options.frames && frame >= options.frames) throw new Error(FRAME_LIMIT);
      if (!options.verify) {
        const wait = t0 + frame * period - performance.now();
        if (wait > 0) Atomics.wait(sleeper, 0, 0, wait);
        else if (wait < -period) lagFrames++;
      }
      lastVblank = performance.now();
      if (frame % 60 === 0) postMessage({ type: "status", text: `frame ${frame}, ${lagFrames} late` });
    },
    disc_info(out) {
      disc.info(new DataView(mem()), out);
      return 1;
    },
    disc_read(lba, count, mode, dst) {
      let at = dst;
      for (let i = 0; i < count; i++) {
        if (!disc.readRaw(lba + i, raw)) return -1;
        const src = mode === 0 ? raw : userData(raw);
        bytes(at, src.length).set(src);
        at += src.length;
      }
      return 0;
    },
    pad_state(port, out) {
      let buttons;
      if (replay) buttons = replayButtons(replay, frame, port & 1);
      else {
        buttons = Atomics.load(pad, 1 + (port & 1));
        if (buttons < 0) return 0;
      }
      new DataView(mem()).setUint16(out, buttons & 0xffff, true);
      return 2;
    },
    storage_read(np, nl, dst, cap) {
      const data = saves.get(text(np, nl));
      if (!data) return -1;
      const n = Math.min(cap, data.length);
      bytes(dst, n).set(data.subarray(0, n));
      return n;
    },
    storage_write(np, nl, src, n) {
      const name = text(np, nl);
      const data = bytes(src, n).slice();
      saves.set(name, data);
      persist(saveDir, name, data).catch((err) => postMessage({ type: "log", level: 3, frame, text: `saving ${name}: ${err}` }));
      return 0;
    },
    report(p, n) {
      postMessage({ type: "report", json: text(p, n) });
    },
    render_submit(p, len) {
      if (!lost) renderer.submit(new Uint32Array(mem().slice(p, p + len)));
    },
    render_readback(x, y, w, h, dst) {
      if (lost) return; // the game reads whatever its buffer held
      const px = renderer.readback(x, y, w, h);
      bytes(dst, px.byteLength).set(new Uint8Array(px.buffer, px.byteOffset, px.byteLength));
    },
    present(p) {
      if (!lost) renderer.present(new Uint32Array(mem().slice(p, p + 20)));
    },
  };

  exports = options.dev ? await instantiateDev(host) : (await WebAssembly.instantiateStreaming(fetch(new URL("./game.wasm", import.meta.url)), { host })).instance.exports;
  const abi = exports.psxw_abi_version();
  if (abi !== ABI_VERSION) throw new Error(`game.wasm ABI ${abi}, host ABI ${ABI_VERSION}`);

  // psxw-abi BootOptions: cd_timing, video, safepoint_budget,
  // safepoint_quantum, debug, trace (u32 each).
  const opts = exports.psxw_alloc(24);
  const words = [options.instantCd ? 1 : 0, options.pal ? 1 : 0, 0, 0, 0, 0];
  words.forEach((w, i) => new DataView(mem()).setUint32(opts + 4 * i, w, true));

  postMessage({ type: "status", text: `booting ${cue}` });
  t0 = performance.now();
  let reason = "exited";
  try {
    exports.psxw_boot(opts);
  } catch (err) {
    const msg = String((err && err.message) || err);
    reason = msg.includes(FRAME_LIMIT) ? "frame limit" : msg;
  }
  disc.close();
  const seconds = (performance.now() - t0) / 1000;
  const sorted = frameTimes.slice().sort((a, b) => a - b);
  const pct = (p) => (sorted.length ? Math.round(sorted[Math.min(sorted.length - 1, Math.floor((p / 100) * sorted.length))] * 100) / 100 : null);
  const frameMs = { p50: pct(50), p95: pct(95), p99: pct(99), max: pct(100) };
  postMessage({ type: "hashes", ram: ramHashes, vram: vramHashes, frames: frame, seconds, lagFrames, frameMs });
  postMessage({ type: "stopped", reason, frame });
}
