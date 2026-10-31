// Cross-host check of the WebGL2 backend (M5): every GPU oracle vector
// (vectors.bin: the renderer command stream and the golden readback, as
// `psxw web-test gpu` exported them) runs through psxw-host-web's
// WebRenderer, and the readback must equal the golden byte for byte. Then
// the context is lost and restored (WEBGL_lose_context) with the largest
// vector's VRAM in it: a new WebRenderer given the shadow copy must hash
// the same (worker.js's recovery). The result is POSTed to
// /result?browser=NAME and shown on the page.

import init, { WebRenderer } from "./pkg/psxw_host_web.js";

const params = new URLSearchParams(location.search);
const browser = params.get("browser") || "unknown";
const out = document.getElementById("out");

async function main() {
  await init({ module_or_path: new URL("./pkg/psxw_host_web_bg.wasm", import.meta.url) });
  const buf = await (await fetch("vectors.bin")).arrayBuffer();
  const dv = new DataView(buf);
  const magic = new TextDecoder().decode(new Uint8Array(buf, 0, 8));
  if (magic !== "PSXWGPUT") throw new Error(`bad vectors.bin (${magic})`);
  const count = dv.getUint32(8, true);
  const canvas = new OffscreenCanvas(1024, 512);
  const r = new WebRenderer(canvas);
  let at = 12;
  let pass = 0;
  const failures = [];
  let largest = null;
  const t0 = performance.now();
  for (let i = 0; i < count; i++) {
    const idLen = dv.getUint32(at, true);
    const id = new TextDecoder().decode(new Uint8Array(buf, at + 4, idLen));
    at += 4 + ((idLen + 3) & ~3);
    const words = dv.getUint32(at, true);
    const stream = new Uint32Array(buf.slice(at + 4, at + 4 + 4 * words));
    at += 4 + 4 * words;
    const n = dv.getUint32(at, true);
    const want = new Uint8Array(buf, at + 4, n);
    at += 4 + ((n + 3) & ~3);
    if (!largest || stream.length > largest.length) largest = stream;
    r.reset();
    const px = r.run(stream);
    const got = new Uint8Array(px.buffer, px.byteOffset, px.byteLength);
    let diff = got.length === want.length ? -1 : Math.min(got.length, want.length);
    for (let j = 0; diff < 0 && j < want.length; j++) if (got[j] !== want[j]) diff = j;
    if (diff < 0) pass++;
    else if (failures.length < 32) failures.push({ id, byte: diff, got: got[diff], want: want[diff] });
  }
  const ms = Math.round(performance.now() - t0);
  const renderer = r.info();
  const upscaling = checkUpscaling(canvas, r);
  const contextLoss = await loseAndRestore(canvas, r, largest);
  const result = { browser, renderer, total: count, pass, failures, ms, upscaling, contextLoss };
  out.textContent = JSON.stringify(result, null, 2);
  await fetch(`/result?browser=${browser}`, { method: "POST", body: JSON.stringify(result) });
}

// Real WebGL draw/read/present coverage at every host scale. A diagonal must
// cut through a native pixel block: nearest-neighbour enlargement cannot pass.
function checkUpscaling(canvas, r) {
  const header = (op, n) => op | (n << 8);
  const xy = (x, y) => x | (y << 16);
  const tri = new Uint32Array(16);
  tri[0] = 1;
  [[2, 2], [14, 2], [2, 14]].forEach(([x, y], i) => {
    tri[3 + 3 * i] = xy(x, y);
    tri[4 + 3 * i] = 255;
  });
  const stream = new Uint32Array([
    header(1, 3), 0, xy(1023, 511), 0,
    header(2, 17), 1, ...tri,
    header(8, 3), 1, 0, xy(16, 16),
    header(5, 4), 0, xy(16, 0), xy(16, 16), 0,
  ]);
  let baseline;
  const checks = [];
  for (const scale of [1, 2, 4]) {
    r.set_resolution_scale(scale);
    r.reset();
    r.run(stream);
    const hash = r.vram_hash();
    baseline ||= hash;
    if (hash !== baseline) throw new Error(`${scale}× changed native VRAM`);
    r.present(new Uint32Array([1, 0, 0, 32, 32]));
    if (canvas.width !== 32 * scale || canvas.height !== 32 * scale) throw new Error(`${scale}× has wrong presentation dimensions`);
    const gl = canvas.getContext("webgl2");
    const px = (x, y) => {
      const p = new Uint8Array(4);
      gl.readPixels(x, canvas.height - 1 - y, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, p);
      return p[0];
    };
    if (px(8 * scale, 7 * scale) !== 248) throw new Error(`${scale}× lost polygon`);
    if (scale > 1 && px(9 * scale - 1, 8 * scale - 1) !== 0) throw new Error(`${scale}× only enlarged native pixels`);
    if (scale > 1 && px(25 * scale - 1, 8 * scale - 1) !== 0) throw new Error(`${scale}× COPY lost enhanced detail`);
    r.present(new Uint32Array([3, 0, 0, 32, 32]));
    if (canvas.width !== 32 || canvas.height !== 32) throw new Error("24-bit video must retain native byte addressing");
    checks.push(scale);
  }
  const before = r.vram_hash();
  for (const scale of [2, 1, 4, 1]) {
    r.set_resolution_scale(scale);
    if (r.vram_hash() !== before) throw new Error("changing scale reset native VRAM");
  }
  return { scales: checks, ok: true };
}

async function loseAndRestore(canvas, r, stream) {
  r.set_resolution_scale(2);
  r.reset();
  r.run(stream);
  const before = r.vram_hash();
  const shadow = r.vram();
  const ext = canvas.getContext("webgl2").getExtension("WEBGL_lose_context");
  if (!ext) return { ok: false, error: "no WEBGL_lose_context" };
  // Resolves true on the event, false after 10 s without it.
  const event = (name) =>
    new Promise((res) => {
      const timer = setTimeout(() => res(false), 10000);
      canvas.addEventListener(
        name,
        (e) => {
          e.preventDefault();
          clearTimeout(timer);
          res(true);
        },
        { once: true },
      );
    });
  const lost = event("webglcontextlost");
  ext.loseContext();
  if (!(await lost)) return { ok: false, error: "no webglcontextlost event" };
  const restored = event("webglcontextrestored");
  ext.restoreContext();
  if (!(await restored)) return { ok: false, unrestored: true, error: "no webglcontextrestored event" };
  const fresh = new WebRenderer(canvas);
  const empty = fresh.vram_hash();
  fresh.upload_vram(shadow);
  fresh.set_resolution_scale(2);
  const after = fresh.vram_hash();
  return { ok: after === before && empty !== before && fresh.resolution_scale() === 2, before, after, scale: fresh.resolution_scale() };
}

main().catch(async (e) => {
  const result = { browser, error: String((e && e.message) || e) };
  out.textContent = JSON.stringify(result, null, 2);
  await fetch(`/result?browser=${browser}`, { method: "POST", body: JSON.stringify(result) });
});
