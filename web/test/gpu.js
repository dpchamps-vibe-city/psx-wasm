// Cross-host check of the WebGL2 backend (M5): every GPU oracle vector
// (vectors.bin: the renderer command stream and the golden readback, as
// `psxw web-test gpu` exported them) runs through psxw-host-web's
// WebRenderer, and the readback must equal the golden byte for byte. The
// result is POSTed to /result?browser=NAME and shown on the page.

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
    r.reset();
    const px = r.run(stream);
    const got = new Uint8Array(px.buffer, px.byteOffset, px.byteLength);
    let diff = got.length === want.length ? -1 : Math.min(got.length, want.length);
    for (let j = 0; diff < 0 && j < want.length; j++) if (got[j] !== want[j]) diff = j;
    if (diff < 0) pass++;
    else if (failures.length < 32) failures.push({ id, byte: diff, got: got[diff], want: want[diff] });
  }
  const result = { browser, renderer: r.info(), total: count, pass, failures, ms: Math.round(performance.now() - t0) };
  out.textContent = JSON.stringify(result, null, 2);
  await fetch(`/result?browser=${browser}`, { method: "POST", body: JSON.stringify(result) });
}

main().catch(async (e) => {
  const result = { browser, error: String((e && e.message) || e) };
  out.textContent = JSON.stringify(result, null, 2);
  await fetch(`/result?browser=${browser}`, { method: "POST", body: JSON.stringify(result) });
});
