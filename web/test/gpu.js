// Cross-host check of the WebGL2 backend (M5): every GPU oracle vector
// (vectors.bin: the renderer command stream and the golden readback, as
// `psxw web-test gpu` exported them) runs through psxw-host-web's
// WebRenderer, and the readback must equal the golden byte for byte. Then
// the context is lost and restored (WEBGL_lose_context) with the largest
// vector's VRAM in it: a new WebRenderer given the shadow copy must hash
// the same (worker.js's recovery). The result is POSTed to
// /result?browser=NAME and shown on the page.

import init, { WebRenderer } from "./pkg/psxw_host_web.js";
import { createRenderingPanel, unpackRenderSettings } from "./rendering.js";

const params = new URLSearchParams(location.search);
const browser = params.get("browser") || "unknown";
const out = document.getElementById("out");
const header = (op, n) => op | (n << 8);
const xy = (x, y) => x | (y << 16);

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
  const textures = checkTextureFiltering(canvas, r);
  const colours = checkColourOptions(canvas, r);
  const sprites = checkSpriteQuads(canvas, r);
  const reconstruction = checkSpriteReconstruction(canvas, r);
  const renderingPanel = checkRenderingPanel();
  const contextLoss = await loseAndRestore(canvas, r, largest);
  const result = { browser, renderer, total: count, pass, failures, ms, upscaling, textures, colours, sprites, reconstruction, renderingPanel, contextLoss };
  out.textContent = JSON.stringify(result, null, 2);
  await fetch(`/result?browser=${browser}`, { method: "POST", body: JSON.stringify(result) });
}

// Real WebGL draw/read/present coverage at every host scale. A diagonal must
// cut through a native pixel block: nearest-neighbour enlargement cannot pass.
function checkUpscaling(canvas, r) {
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

function checkTextureFiltering(canvas, r) {
  const triangle = new Uint32Array(16);
  triangle[0] = 1 | 8 | 16 | (2 << 12);
  [[2, 2, 0, 64], [14, 2, 3, 64], [2, 14, 0, 67]].forEach(([x, y, u, v], i) => {
    triangle[3 + 3 * i] = xy(x, y);
    triangle[5 + 3 * i] = u | (v << 8);
  });
  const stream = new Uint32Array([
    header(1, 3), 0, xy(1023, 511), 0,
    header(4, 19), xy(0, 64), xy(8, 4), 0, ...Array(16).fill(0x03e0001f),
    header(8, 3), 1, xy(0, 64), xy(8, 4),
    header(2, 17), 1, ...triangle,
  ]);
  let baseline;
  for (const scale of [1, 2, 4]) {
    for (const [textures, precision] of [[0, 0], [1, 0], [0, 1], [1, 1]]) {
      r.set_visual_options(1, scale, textures, 0, precision);
      r.reset();
      r.run(stream);
      const hash = r.vram_hash();
      baseline ||= hash;
      if (hash !== baseline) throw new Error("texture filtering changed game-visible VRAM");
      r.present(new Uint32Array([1, 0, 0, 32, 32]));
      const gl = canvas.getContext("webgl2");
      const p = new Uint8Array(4);
      gl.readPixels(5 * scale, canvas.height - 1 - 3 * scale, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, p);
      const want = textures ? (precision ? [62, 186, 0, 255] : [64, 184, 0, 255]) : [0, 248, 0, 255];
      if (!p.every((v, i) => v === want[i])) throw new Error(`${scale}× texture filter ${textures}: got ${p}, want ${want}`);
    }
  }
  return { ok: true, scales: [1, 2, 4], modes: ["nearest", "smooth-3d"] };
}

function checkColourOptions(canvas, r) {
  const flat = new Uint32Array(16), gradient = new Uint32Array(16);
  flat[0] = 1 | 512;
  gradient[0] = 1 | 32 | 512;
  [[2, 2], [14, 2], [2, 14]].forEach(([x, y], i) => {
    flat[3 + 3 * i] = xy(x, y);
    flat[4 + 3 * i] = 0x808080;
    gradient[3 + 3 * i] = xy(x + 16, y);
    gradient[4 + 3 * i] = i === 1 ? 0xe0e0e0 : 0x202020;
  });
  const stream = new Uint32Array([
    header(1, 3), 0, xy(1023, 511), 0,
    header(2, 33), 2, ...flat, ...gradient,
  ]);
  r.set_resolution_scale(1);
  r.reset();
  r.run(stream);
  const before = r.vram_hash();
  const gl = canvas.getContext("webgl2");
  const px = (x, y) => {
    const p = new Uint8Array(4);
    gl.readPixels(x, canvas.height - 1 - y, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, p);
    return p[0];
  };
  for (const precision of [0, 1]) {
    for (const dithering of [0, 1]) {
      r.set_visual_options(1, 4, 0, dithering, precision);
      r.present(new Uint32Array([1, 0, 0, 32, 32]));
      if (px(16, 16) !== 120) throw new Error("colour-plane seeding changed native pixel colours");
      r.reset();
      r.run(stream);
      if (r.vram_hash() !== before) throw new Error("colour options changed native VRAM");
      r.present(new Uint32Array([1, 0, 0, 32, 32]));
      const low = dithering ? 128 : precision ? 124 : 120;
      const shade = precision ? (dithering ? 52 : 50) : 48;
      if (px(16, 16) !== low || px(20, 16) !== 128 || px(77, 12) !== shade) throw new Error(`wrong dithering/precision output (${dithering}/${precision})`);
      r.present(new Uint32Array([3, 0, 0, 32, 32]));
      if (canvas.width !== 32 || canvas.height !== 32) throw new Error("high precision changed packed-video addressing");
    }
  }
  return { ok: true, dithering: ["original", "off"], precision: ["native", "high"] };
}

function checkSpriteQuads(canvas, r) {
  const quad = new Uint32Array(16);
  quad[0] = 2 | 8 | 16 | (2 << 12);
  [[2, 2, 16, 64], [10, 2, 24, 64], [2, 10, 16, 72], [10, 10, 24, 72]].forEach(([x, y, u, v], i) => {
    quad[3 + 3 * i] = xy(x, y);
    quad[5 + 3 * i] = u | (v << 8);
  });
  const texels = new Uint16Array(64).fill(0x03e0);
  texels[4 * 8 + 4] = 0; // A cutout must stay pixel-centred, not shift by half a pixel.
  const packed = Array.from({length: 32}, (_, i) => texels[i * 2] | (texels[i * 2 + 1] << 16));
  const stream = new Uint32Array([
    header(1, 3), 0, xy(1023, 511), 0,
    header(3, 3), 0, xy(32, 32), 0x7c00,
    header(4, 35), xy(16, 64), xy(8, 8), 0, ...packed,
    header(8, 3), 1, xy(16, 64), xy(8, 8),
    header(2, 17), 1, ...quad,
  ]);
  const gl = canvas.getContext("webgl2");
  const pixels = () => {
    const out = new Uint8Array(canvas.width * canvas.height * 4);
    gl.readPixels(0, 0, canvas.width, canvas.height, gl.RGBA, gl.UNSIGNED_BYTE, out);
    return out;
  };
  r.set_resolution_scale(1);
  r.reset();
  r.run(stream);
  const hash = r.vram_hash();
  r.present(new Uint32Array([1, 0, 0, 32, 32]));
  const reference = pixels();
  for (const scale of [2, 4]) {
    for (const textures of [0, 1]) {
      for (const precision of [0, 1]) {
        r.set_visual_options(1, scale, textures, 1, precision);
        r.reset();
        r.run(stream);
        if (r.vram_hash() !== hash) throw new Error("sprite fix changed native VRAM");
        r.present(new Uint32Array([1, 0, 0, 32, 32]));
        const got = pixels();
        for (let y = 0; y < canvas.height; y++) {
          for (let x = 0; x < canvas.width; x++) {
            const at = (y * canvas.width + x) * 4;
            const ref = (Math.floor(y / scale) * 32 + Math.floor(x / scale)) * 4;
            for (let c = 0; c < 4; c++) if (got[at + c] !== reference[ref + c]) throw new Error(`sprite seam/cutout shift at ${scale}× (${x}, ${y})`);
          }
        }
      }
    }
  }
  return { ok: true, scales: [2, 4] };
}

function checkSpriteReconstruction(canvas, r) {
  const quad = new Uint32Array(16);
  quad[0] = 2 | 8 | 16 | (2 << 12);
  [[2, 2, 16, 64], [10, 2, 24, 64], [2, 10, 16, 72], [10, 10, 24, 72]].forEach(([x, y, u, v], i) => {
    quad[3 + 3*i] = xy(x, y);
    quad[5 + 3*i] = u | (v << 8);
  });
  const texels = Array.from({ length: 64 }, (_, i) => i % 8 >= Math.floor(i / 8) ? 0x001f : 0);
  const packed = Array.from({ length: 32 }, (_, i) => texels[2*i] | (texels[2*i+1] << 16));
  const stream = new Uint32Array([
    header(1, 3), 0, xy(1023, 511), 0,
    header(3, 3), 0, xy(32, 32), 0x7c00,
    header(4, 35), xy(16, 64), xy(8, 8), 0, ...packed,
    header(8, 3), 1, xy(16, 64), xy(8, 8),
    header(2, 17), 1, ...quad,
  ]);
  r.set_resolution_scale(1);
  r.reset();
  r.run(stream);
  const hash = r.vram_hash();
  const gl = canvas.getContext("webgl2");
  for (const scale of [2, 4]) {
    for (const [precision, sprites] of [[0, 1], [1, 1], [0, 2], [1, 2], [0, 3], [1, 3], [0, 4], [1, 4]]) {
      r.set_visual_options(1, scale, 1, 1, precision, sprites);
      r.reset();
      r.run(stream);
      if (r.vram_hash() !== hash) throw new Error("sprite reconstruction changed native VRAM");
      r.present(new Uint32Array([1, 0, 0, 32, 32]));
      let partial = 0, expanded = 0;
      for (let y = 0; y < 8*scale; y++) {
        for (let x = 0; x < 8*scale; x++) {
          const p = new Uint8Array(4);
          gl.readPixels(2*scale+x, canvas.height-1-(2*scale+y), 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, p);
          if (p[1] !== 0 || Math.abs(p[0]+p[2]-248) > 8) throw new Error(`sprite filter ${sprites} fringe: ${p}`);
          if (sprites === 2 && ![0, 248].includes(p[0])) throw new Error(`MMPX introduced a non-palette colour: ${p}`);
          if (p[0] > 0 && p[0] < 248 && p[2] > 0) partial++;
          if (Math.floor(x/scale) < Math.floor(y/scale) && p[0] > 0) expanded++;
        }
      }
      if (!expanded || (sprites === 2 ? partial !== 0 : !partial)) throw new Error(`wrong sprite reconstruction: ${sprites}`);
    }
  }
  return { ok: true, scales: [2, 4], modes: ["xbr", "mmpx", "bicubic", "super-xbr-3p"] };
}

function checkRenderingPanel() {
  const key = "psxw.rendering", saved = localStorage.getItem(key);
  const panel = document.createElement("div");
  document.body.append(panel);
  try {
    localStorage.setItem(key, JSON.stringify({ renderer: "clean-polygons", options: {
      "clean-polygons": { scale: 4, textures: 1, dithering: 1, precision: 1 },
    } }));
    const ui = createRenderingPanel(panel, new URLSearchParams());
    const word = () => Atomics.load(new Int32Array(ui.buffer), 0);
    const before = word();
    const control = panel.querySelector("#sprite-reconstruction");
    if (panel.querySelector("#visual-renderer").selectedOptions[0].text !== "PSXRevamp") throw new Error("renderer rename not migrated");
    for (const mode of [1, 2, 3, 4]) {
      control.value = String(mode);
      control.dispatchEvent(new Event("change"));
      const config = unpackRenderSettings(word());
      if (config.sprites !== mode || config.textures !== 1 || config.scale !== 4 || config.dithering !== 1 || config.precision !== 1) throw new Error("sprite option replaced another rendering setting");
      const persisted = createRenderingPanel(document.createElement("div"), new URLSearchParams());
      if (unpackRenderSettings(Atomics.load(new Int32Array(persisted.buffer), 0)).sprites !== mode) throw new Error("sprite mode did not persist");
      ui.accept(before); // A late ack must not undo reconstruction.
      if (control.value !== String(mode)) throw new Error("stale acknowledgement overwrote sprite option");
      Atomics.store(new Int32Array(ui.buffer), 0, before); // Failed factory rolls back atomically.
      ui.accept(before);
      if (control.value !== "0") throw new Error("failed reconstruction change did not roll back");
    }
    for (const [name, mode] of [["xbr", 1], ["mmpx", 2], ["bicubic", 3], ["super-xbr-3p", 4]]) {
      const restored = createRenderingPanel(panel, new URLSearchParams(`sprites=${name}`));
      const config = unpackRenderSettings(Atomics.load(new Int32Array(restored.buffer), 0));
      if (config.sprites !== mode || config.style !== 1) throw new Error(`${name} query override not applied`);
    }
    return { ok: true };
  } finally {
    panel.remove();
    if (saved === null) localStorage.removeItem(key); else localStorage.setItem(key, saved);
  }
}

async function loseAndRestore(canvas, r, stream) {
  r.set_visual_options(1, 2, 1, 1, 1, 4);
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
  fresh.set_visual_options(1, 2, 1, 1, 1, 4);
  const after = fresh.vram_hash();
  return { ok: after === before && empty !== before && fresh.resolution_scale() === 2, before, after, scale: fresh.resolution_scale() };
}

main().catch(async (e) => {
  const result = { browser, error: String((e && e.message) || e) };
  out.textContent = JSON.stringify(result, null, 2);
  await fetch(`/result?browser=${browser}`, { method: "POST", body: JSON.stringify(result) });
});
