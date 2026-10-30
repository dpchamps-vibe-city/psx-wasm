// Cross-host check without a disc (M5): each conformance scenario
// (`psxw conform`), recorded on the native host step by step with its
// results, is replayed here on the same game.wasm, with the WebRenderer
// on an OffscreenCanvas and a host like the native one in a conformance
// run (no disc, no storage, pads from the replay). Every call's return,
// every read, and the final VRAM and audio hashes must equal the native
// ones. The result is POSTed to /result?browser=NAME and shown on the page.

import init, { WebRenderer } from "./pkg/psxw_host_web.js";

const ABI_VERSION = 12;
const NOTHING_PRESSED = 0xffff;
const REPLAY_HEADER = 64; // version 2, entries by vblank (clock 0)
const WINDOW_BASE = 0x01000000;
const WINDOW_MASK = 0x00ffffff;

const browser = new URLSearchParams(location.search).get("browser") || "unknown";
const out = document.getElementById("out");
const hex = (b) => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
const unhex = (s) => new Uint8Array(s.match(/../g)?.map((h) => parseInt(h, 16)) ?? []);

/** FNV-1a 64 in 32-bit halves, as web/worker.js. */
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
      hi = (hi * 0x1b3 + Math.floor(p / 0x100000000) + ((lo << 8) >>> 0)) % 0x100000000;
      lo = p >>> 0;
    }
    this.hi = hi;
    this.lo = lo;
  }
  hex() {
    return this.hi.toString(16).padStart(8, "0") + this.lo.toString(16).padStart(8, "0");
  }
}

async function scenario(module, rec, ram, replay) {
  const renderer = new WebRenderer(new OffscreenCanvas(640, 480));
  const audio = new Fnv64();
  let audioFrames = 0;
  let frame = 0;
  let exports = null;
  const mem = () => exports.memory.buffer;
  const bytes = (p, n) => new Uint8Array(mem(), p, n);
  const text = (p, n) => new TextDecoder().decode(bytes(p, n).slice());
  const guest = (addr, n) => bytes(WINDOW_BASE + (addr & WINDOW_MASK), n);
  const host = {
    log() {},
    trap(code, p, n) {
      throw new Error(`runtime trap ${code}: ${text(p, n)}`);
    },
    vblank(f) {
      frame = Number(f);
    },
    wait_vblank() {},
    disc_info: () => 0,
    disc_read: () => -1,
    pad_state(port, outp) {
      const dv = new DataView(replay.buffer, replay.byteOffset, replay.byteLength);
      const buttons = frame < dv.getUint32(16, true) ? dv.getUint16(REPLAY_HEADER + 4 * frame + 2 * (port & 1), true) : NOTHING_PRESSED;
      new DataView(mem()).setUint16(outp, buttons, true);
      return 2;
    },
    storage_read: () => -1,
    storage_write: () => -1,
    report() {},
    render_submit(p, len) {
      renderer.submit(new Uint32Array(mem().slice(p, p + len)));
    },
    render_readback(x, y, w, h, dst) {
      const px = renderer.readback(x, y, w, h);
      bytes(dst, px.byteLength).set(new Uint8Array(px.buffer, px.byteOffset, px.byteLength));
    },
    present(p) {
      renderer.present(new Uint32Array(mem().slice(p, p + 20)));
    },
    gpu_record_begin() {
      return 0;
    },
    gpu_record() {},
    spu_record_begin() {
      return 0;
    },
    spu_record() {},
    audio(p, frames) {
      audio.bytes(bytes(p, frames * 4));
      audioFrames += frames;
    },
  };
  exports = (await WebAssembly.instantiate(module, { host })).exports;
  if (exports.psxw_abi_version() !== ABI_VERSION) throw new Error(`game.wasm ABI ${exports.psxw_abi_version()}`);
  guest(rec.ram_addr, ram.length).set(ram);
  const opts = exports.psxw_alloc(24);
  new Uint8Array(mem(), opts, 24).fill(0);
  exports.psxw_conform_init(opts, rec.sp, rec.gp);

  const diffs = [];
  rec.steps.forEach((s, i) => {
    if (diffs.length >= 16) return;
    if (s.op === "write") guest(s.addr, s.hex.length / 2).set(unhex(s.hex));
    else if (s.op === "read") {
      const got = hex(guest(s.addr, s.len));
      if (got !== s.hex) diffs.push({ step: i, op: "read", addr: s.addr, got, want: s.hex });
    } else if (s.op === "vblanks") exports.psxw_run_vblanks(s.n);
    else if (s.op === "call") {
      s.args.slice(4).forEach((a, k) => new DataView(mem()).setUint32(WINDOW_BASE + ((rec.sp + 4 * (k + 4)) & WINDOW_MASK), a, true));
      const a = (k) => s.args[k] ?? 0;
      const got = "0x" + (BigInt(exports.psxw_call(s.addr, a(0), a(1), a(2), a(3))) & 0xffffffffffffffffn).toString(16);
      if (got !== s.ret) diffs.push({ step: i, op: "call", addr: s.addr, got, want: s.ret });
    }
  });
  return { library: rec.library, steps: rec.steps.length, diffs, vram: renderer.vram_hash(), audio: { frames: audioFrames, hash: audio.hex() } };
}

async function main() {
  await init({ module_or_path: new URL("./pkg/psxw_host_web_bg.wasm", import.meta.url) });
  const module = await WebAssembly.compileStreaming(fetch("game.wasm"));
  const recs = await (await fetch("conform.json")).json();
  const ram = new Uint8Array(await (await fetch("ram.bin")).arrayBuffer());
  const replay = new Uint8Array(await (await fetch("replay.rpl")).arrayBuffer());
  const results = [];
  for (const rec of recs) {
    try {
      results.push(await scenario(module, rec, ram, replay));
    } catch (e) {
      results.push({ library: rec.library, error: String((e && e.message) || e) });
    }
  }
  return { browser, results };
}

main()
  .catch((e) => ({ browser, error: String((e && e.message) || e) }))
  .then(async (result) => {
    out.textContent = JSON.stringify(result, null, 2);
    await fetch(`/result?browser=${browser}`, { method: "POST", body: JSON.stringify(result) });
  });
