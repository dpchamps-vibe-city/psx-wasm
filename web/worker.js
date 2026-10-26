// The browser host (M5): runs game.wasm in this Worker and implements its
// host interface (crates/psxw-abi: `host.*`, plus the M4 render imports):
// - pacing: `wait_vblank` sleeps with Atomics.wait until the frame's
//   wall-clock deadline (no pacing in verify mode);
// - the disc: a .cue/.bin image in OPFS (`disc/`), sync access handles;
// - pads: the main thread's SharedArrayBuffer, or a replay file;
// - storage (memory cards): files in OPFS `saves/` (cards.js), loaded at
//   start and written back as the game saves;
// - rendering: psxw-host-web's WebRenderer (psx-render-gl on WebGL2) on
//   this Worker's own OffscreenCanvas. The game never returns to the
//   event loop, so a canvas transferred from the page would never show a
//   frame; each presented frame goes to the page as an ImageBitmap
//   instead (at most two in flight; the page counts what it shows in the
//   shared buffer's word 4);
// - audio: `host.audio` frames into the AudioWorklet's SharedArrayBuffer
//   ring (audio.js), hashed as the native host does (FNV-1a over the
//   frames' bytes).
//
// - dev mode (`options.dev`): guest.wasm and runtime.wasm instantiated
//   separately and wired through JS trampolines, so a rebuilt runtime is
//   picked up without relinking the game (slower: every runtime call
//   crosses JS);
// - WebGL context loss: VRAM is shadowed every 60 frames and uploaded
//   into the re-created renderer when the context is restored (the
//   colour-table cache refills on the next table change).
//
// Messages in: {type: "start", cue, padBuffer, audioRing, options}.
// Messages out: {type: "status"|"report"|"log"|"frame"|"replay"|"hashes"|"stopped", ...}.

import init, { WebRenderer } from "./pkg/psxw_host_web.js";
import { Disc, SECTOR, userData } from "./disc.js";
import { loadSaves, persist, savesDir } from "./cards.js";

const ABI_VERSION = 6;
const NOTHING_PRESSED = 0xffff;
const WINDOW_BASE = 0x01000000;
const FRAME_LIMIT = "psxw: frame limit";

onmessage = async (e) => {
  if (e.data.type !== "start") return;
  try {
    await run(e.data);
  } catch (err) {
    postMessage({ type: "stopped", reason: String((err && err.message) || err) });
  }
};

/**
 * Pad buttons for `port` at vblank `frame` from a replay (psxw-abi
 * replay). Version 2 replays may name a clock: a guest u32 frame counter
 * whose value (read with `readU32`) picks the entry.
 */
function replayButtons(replay, frame, port, readU32) {
  const dv = new DataView(replay.buffer, replay.byteOffset, replay.byteLength);
  const version = dv.getUint32(8, true);
  const entries = dv.getUint32(16, true);
  const data = version === 1 ? 56 : 64;
  const clock = version === 1 ? 0 : dv.getUint32(56, true);
  const at = clock ? readU32(clock) : frame;
  if (at >= entries) return NOTHING_PRESSED;
  return dv.getUint16(data + 4 * at + 2 * port, true);
}

/**
 * A recorded session as a version 2 replay (psxw-abi replay): entry `c`
 * holds the pads the game saw at the last vblank its frame counter (the
 * clock) read `c`, which is the state it polls.
 */
function encodeReplay(rec, gameId) {
  const n = Math.max(rec.frames, rec.entries.length);
  const out = new Uint8Array(64 + 4 * n);
  const dv = new DataView(out.buffer);
  out.set(new TextEncoder().encode("PSXWRPL\0"), 0);
  dv.setUint32(8, 2, true);
  dv.setUint32(12, 0x41, true);
  dv.setUint32(16, n, true);
  out.set(new TextEncoder().encode(gameId.slice(0, 15)), 20);
  dv.setUint32(56, rec.clock, true);
  for (let i = 0; i < n; i++) {
    const e = rec.entries[i] || [NOTHING_PRESSED, NOTHING_PRESSED];
    dv.setUint16(64 + 4 * i, e[0], true);
    dv.setUint16(66 + 4 * i, e[1], true);
  }
  return out;
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

/** FNV-1a 64 over bytes, in two 32-bit halves (the prime is 2^40 + 1B3h). */
class Fnv64 {
  constructor() {
    this.hi = 0xcbf29ce4;
    this.lo = 0x84222325;
  }
  bytes(b) {
    let { hi, lo } = this;
    for (let i = 0; i < b.length; i++) {
      lo = (lo ^ b[i]) >>> 0;
      const p = lo * 0x1b3;
      const carry = Math.floor(p / 0x100000000);
      hi = (hi * 0x1b3 + carry + ((lo << 8) >>> 0)) % 0x100000000;
      lo = p >>> 0;
    }
    this.hi = hi;
    this.lo = lo;
  }
  hex() {
    return this.hi.toString(16).padStart(8, "0") + this.lo.toString(16).padStart(8, "0");
  }
}

async function run({ cue, padBuffer, audioRing, options }) {
  const canvas = new OffscreenCanvas(options.width || 640, options.height || 480);
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
  // [seq, port 1 buttons, port 2 buttons or -1, save-replay requests, frames shown]
  const pad = new Int32Array(padBuffer);
  if (pad.length < 5) throw new Error("this page is older than the Worker (the bundle was rebuilt): reload the page");
  let framesPosted = 0;
  const recording = options.record ? { clock: options.clock >>> 0, entries: [], frames: 0, saved: 0 } : null;
  const postReplay = () => {
    const bytes = encodeReplay(recording, options.gameId || "");
    postMessage({ type: "replay", bytes, frames: recording.frames }, [bytes.buffer]);
  };
  const sleeper = new Int32Array(new SharedArrayBuffer(4));
  const period = options.pal ? 1000 / 49.761 : 1000 / 59.826;
  const hashAt = new Set(options.hashAt || []);
  const ramHashes = {};
  const vramHashes = {};
  let lagFrames = 0;
  const audioHash = new Fnv64();
  let audioFrames = 0;
  const ringHead = audioRing ? new Int32Array(audioRing, 0, 2) : null;
  const ringSamples = audioRing ? new Int16Array(audioRing, 8) : null;
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
      if (recording) {
        recording.frames = frame;
        const asked = Atomics.load(pad, 3);
        if (asked !== recording.saved) {
          recording.saved = asked;
          postReplay();
        }
      }
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
      if (replay) buttons = replayButtons(replay, frame, port & 1, (a) => new DataView(mem()).getUint32(WINDOW_BASE + (a & 0x1ffffc), true));
      else buttons = Atomics.load(pad, 1 + (port & 1));
      if (recording) {
        // Before the game zeroes its counter the word is garbage; a
        // replay reads such values as nothing pressed too.
        const c = new DataView(mem()).getUint32(WINDOW_BASE + (recording.clock & 0x1ffffc), true);
        if (c <= frame) {
          const e = (recording.entries[c] ||= [NOTHING_PRESSED, NOTHING_PRESSED]);
          e[port & 1] = buttons < 0 ? NOTHING_PRESSED : buttons & 0xffff;
        }
      }
      if (buttons < 0) return 0;
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
      if (lost) return;
      renderer.present(new Uint32Array(mem().slice(p, p + 20)));
      // A page that falls behind misses frames rather than queueing them.
      if (framesPosted - Atomics.load(pad, 4) < 2) {
        const bitmap = canvas.transferToImageBitmap();
        framesPosted++;
        postMessage({ type: "frame", frame, bitmap }, [bitmap]);
      }
    },
    // Recording (ABI 6) is the native host's; the browser never asks.
    gpu_record_begin() {
      return 0;
    },
    gpu_record() {},
    spu_record_begin() {
      return 0;
    },
    spu_record() {},
    audio(p, frames) {
      const b = bytes(p, frames * 4);
      audioHash.bytes(b);
      audioFrames += frames;
      if (!ringHead) return;
      // Frames the worklet hasn't played yet are kept; overflow drops new.
      const s = new Int16Array(b.slice().buffer);
      const size = ringSamples.length / 2;
      let w = Atomics.load(ringHead, 0);
      const r = Atomics.load(ringHead, 1);
      for (let i = 0; i < frames; i++) {
        const next = (w + 1) % size;
        if (next === r) break;
        ringSamples[2 * w] = s[2 * i];
        ringSamples[2 * w + 1] = s[2 * i + 1];
        w = next;
      }
      Atomics.store(ringHead, 0, w);
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
  // A recording outlives its run: hand it over when the run stops.
  if (recording && recording.entries.length) postReplay();
  const seconds = (performance.now() - t0) / 1000;
  const sorted = frameTimes.slice().sort((a, b) => a - b);
  const pct = (p) => (sorted.length ? Math.round(sorted[Math.min(sorted.length - 1, Math.floor((p / 100) * sorted.length))] * 100) / 100 : null);
  const frameMs = { p50: pct(50), p95: pct(95), p99: pct(99), max: pct(100) };
  const audio = { frames: audioFrames, hash: audioHash.hex() };
  postMessage({ type: "hashes", ram: ramHashes, vram: vramHashes, frames: frame, seconds, lagFrames, frameMs, audio });
  postMessage({ type: "stopped", reason, frame });
}
