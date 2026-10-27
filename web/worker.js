// The browser host (M5): runs game.wasm in this Worker and implements its
// host interface (crates/psxw-abi: `host.*`, plus the M4 render imports):
// - pacing: `wait_vblank` sleeps with Atomics.wait until the frame's
//   wall-clock deadline (no pacing in verify mode). Where the browser has
//   JSPI it first suspends the game and gives this Worker's event loop a
//   turn: Chromium frees the frames posted to the page only there (without
//   it, memory grows by a frame a vblank until the GPU process dies), and
//   context-loss events land there;
// - the disc: a .cue/.bin image in OPFS (`disc/`), sync access handles;
// - pads: the main thread's SharedArrayBuffer, or a replay file;
// - storage (memory cards): files in OPFS `saves/` (cards.js), loaded at
//   start; each card the game changes goes to the page, which writes it;
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
//   colour-table cache refills on the next table change). Without JSPI
//   the events never arrive.
//
// - mods (M11 phase 2, mods.js): with a moddable base, mod modules load
//   before boot (`options.mods`: URLs, or `options.modsFromOpfs`: the
//   enabled ones in OPFS `mods/`), swap in at given vblanks
//   (`options.swaps`), or arrive while the game runs through a shared
//   mailbox (`modBuffer`), applied at the next vblank.
//
// Messages in: {type: "start", cue, padBuffer, audioRing, modBuffer, options},
// {type: "execute", id, address, args} (one call, at a borrow-free boundary).
// Messages out: {type: "status"|"report"|"log"|"frame"|"save"|"replay"|"hashes"|"mods"|"scripting-ready"|"execute-result"|"stopped", ...}.

import init, { WebRenderer } from "./pkg/psxw_host_web.js";
import { Disc, SECTOR, userData } from "./disc.js";
import { loadSaves, savesDir } from "./cards.js";
import { ModLoader, takeMod } from "./mods.js";

const ABI_VERSION = 8;
const NOTHING_PRESSED = 0xffff;
const WINDOW_BASE = 0x01000000;
const FRAME_LIMIT = "psxw: frame limit";

let enqueueExecution = null;
onmessage = async (e) => {
  if (e.data.type === "execute") {
    try {
      if (!enqueueExecution) throw new Error("scripting is unavailable (game not running, no JSPI, or dev mode)");
      enqueueExecution(e.data);
    } catch (err) {
      postMessage({ type: "execute-result", id: e.data.id, error: String(err.message || err) });
    }
    return;
  }
  if (e.data.type !== "start") return;
  try {
    await run(e.data);
  } catch (err) {
    enqueueExecution = null;
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

/** The enabled mods in OPFS `mods/`, in their order (main.js keeps them). */
async function opfsMods() {
  const root = await navigator.storage.getDirectory();
  const dir = await root.getDirectoryHandle("mods", { create: true });
  let order = [];
  try {
    order = JSON.parse(await (await (await dir.getFileHandle("order.json")).getFile()).text());
  } catch {}
  const out = [];
  for (const m of order.filter((m) => m.enabled)) {
    out.push({ name: m.file, bytes: new Uint8Array(await (await (await dir.getFileHandle(m.file)).getFile()).arrayBuffer()) });
  }
  return out;
}

async function run({ cue, padBuffer, audioRing, modBuffer, options }) {
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
  const saves = await loadSaves(await savesDir());
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

  // The game yields once a vblank where JSPI exists. Dev mode can't: its
  // JS trampolines sit between the game and wait_vblank, and JSPI
  // suspends wasm frames only.
  const suspend = !options.dev && typeof WebAssembly.Suspending === "function" && typeof WebAssembly.promising === "function";
  const turn = new MessageChannel();
  const nextTask = () =>
    new Promise((resolve) => {
      turn.port1.onmessage = resolve;
      turn.port2.postMessage(null);
    });
  let scriptingReady = false;
  // Mods: the loader exists once the base is instantiated.
  let loader = null;
  const swaps = [];
  let mailboxSeq = modBuffer ? Atomics.load(new Int32Array(modBuffer, 0, 1), 0) : 0;
  const modsChanged = (what) => postMessage({ type: "mods", ids: loader.ids(), functions: loader.functions(), what, frame });
  const vblank = (f) => {
    frame = Number(f);
    if (loader) {
      for (const w of swaps.filter((w) => w.frame === frame)) modsChanged(`swapped ${loader.swap(w.index, w.bytes)} in at vblank ${frame}`);
      const pushed = modBuffer && takeMod(modBuffer, mailboxSeq);
      if (pushed) {
        mailboxSeq = pushed.seq;
        try {
          modsChanged(`swapped ${loader.swap(pushed.index, pushed.bytes)} in at vblank ${frame}`);
        } catch (err) {
          postMessage({ type: "status", text: `mod swap failed: ${err.message || err}` });
        }
      }
    }
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
  };
  const pace = () => {
    if (!options.verify) {
      const wait = t0 + frame * period - performance.now();
      if (wait > 0) Atomics.wait(sleeper, 0, 0, wait);
      else if (wait < -period) lagFrames++;
    }
    lastVblank = performance.now();
    if (frame % 60 === 0) postMessage({ type: "status", text: `frame ${frame}, ${lagFrames} late` });
  };

  const host = {
    wait_vblank: suspend
      ? new WebAssembly.Suspending(async (f) => {
          vblank(f);
          if (!scriptingReady) {
            scriptingReady = true;
            postMessage({ type: "scripting-ready" });
          }
          await nextTask();
          pace();
        })
      : (f) => {
          vblank(f);
          pace();
        },
    log(level, p, n) {
      postMessage({ type: "log", level, frame, text: text(p, n) });
    },
    trap(code, p, n) {
      throw new Error(`runtime trap ${code}: ${text(p, n)}`);
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
      // The page writes it: its event loop runs, this Worker's may not.
      postMessage({ type: "save", name, bytes: data });
      return 0;
    },
    report(p, n) {
      const json = text(p, n);
      const event = JSON.parse(json);
      if (loader && event.event === "boot") loader.booted = true;
      if (event.event === "execute") postMessage({ type: "execute-result", id: event.id, value: event.value });
      else postMessage({ type: "report", json });
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

  let baseModule = null;
  if (options.dev) exports = await instantiateDev(host);
  else {
    const r = await WebAssembly.instantiateStreaming(fetch(new URL("./game.wasm", import.meta.url)), { host });
    baseModule = r.module;
    exports = r.instance.exports;
  }
  const abi = exports.psxw_abi_version();
  if (abi !== ABI_VERSION) throw new Error(`game.wasm ABI ${abi}, host ABI ${ABI_VERSION}`);
  if (suspend) {
    enqueueExecution = ({ id, address, args }) => {
      const a = (i) => args[i] ?? 0;
      const status = exports.psxw_execute(id, address, a(0), a(1), a(2), a(3));
      if (status !== 0) throw new Error(`execute rejected: ${status === -2 ? "another call is pending" : status === -3 ? "invalid address" : "runtime not ready"}`);
    };
  } else {
    postMessage({ type: "scripting-ready", error: "scripting needs JSPI and the merged game.wasm (not ?dev)" });
  }

  // Mods, in order, before boot; swaps fetched now, applied at their vblank.
  const fetchBytes = async (u) => new Uint8Array(await (await fetch(new URL(u, location.href))).arrayBuffer());
  const wanted = [];
  for (const u of options.mods || []) wanted.push({ name: u, bytes: await fetchBytes(u) });
  if (options.modsFromOpfs) wanted.push(...(await opfsMods()));
  for (const w of options.swaps || []) swaps.push({ frame: w.frame, index: w.index, bytes: await fetchBytes(w.url) });
  if (wanted.length || swaps.length || modBuffer) {
    loader = baseModule && ModLoader.forBase(baseModule, exports);
    if (!loader && (wanted.length || swaps.length)) throw new Error("this bundle's game.wasm isn't a moddable base (psxw build-web --moddable)");
    if (loader) {
      for (const m of wanted) loader.load(m.bytes);
      modsChanged(loader.mods.length ? `loaded ${loader.ids().join(", ")}` : "no mods");
    }
  }

  // psxw-abi BootOptions: cd_timing, video, safepoint_budget,
  // safepoint_quantum, debug, trace (u32 each).
  const opts = exports.psxw_alloc(24);
  const words = [options.instantCd ? 1 : 0, options.pal ? 1 : 0, 0, 0, 0, 0];
  words.forEach((w, i) => new DataView(mem()).setUint32(opts + 4 * i, w, true));

  postMessage({ type: "status", text: `booting ${cue}` });
  postMessage({
    type: "log",
    level: suspend ? 1 : 2,
    frame: 0,
    text: suspend
      ? "the game yields to the Worker's event loop every vblank (JSPI)"
      : "no JSPI here (or dev mode): the Worker never yields, so Chromium's memory grows with every frame shown",
  });
  t0 = performance.now();
  let reason = "exited";
  try {
    await (suspend ? WebAssembly.promising(exports.psxw_boot) : exports.psxw_boot)(opts);
  } catch (err) {
    const msg = String((err && err.message) || err);
    reason = msg.includes(FRAME_LIMIT) ? "frame limit" : msg;
  }
  enqueueExecution = null;
  disc.close();
  // A recording outlives its run: hand it over when the run stops.
  if (recording && recording.entries.length) postReplay();
  const seconds = (performance.now() - t0) / 1000;
  const sorted = frameTimes.slice().sort((a, b) => a - b);
  const pct = (p) => (sorted.length ? Math.round(sorted[Math.min(sorted.length - 1, Math.floor((p / 100) * sorted.length))] * 100) / 100 : null);
  const frameMs = { p50: pct(50), p95: pct(95), p99: pct(99), max: pct(100) };
  const audio = { frames: audioFrames, hash: audioHash.hex() };
  postMessage({ type: "hashes", ram: ramHashes, vram: vramHashes, frames: frame, seconds, lagFrames, frameMs, audio, yields: suspend });
  postMessage({ type: "stopped", reason, frame });
}
