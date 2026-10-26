// Memory cards through OPFS (M7): the page's import and export
// (cards.js) round-trip an image byte for byte and refuse a wrong size;
// the Worker's side (storage-worker.js, as worker.js does it) loads what
// the page imported and persists a save the page then exports. The result
// is POSTed to /result?browser=NAME and shown on the page.

import { CARD_SIZE, exportCard, importCard, savesDir } from "./cards.js";

const browser = new URLSearchParams(location.search).get("browser") || "unknown";
const out = document.getElementById("out");

/** A card-sized image whose every byte depends on its offset and `seed`. */
function pattern(seed) {
  const b = new Uint8Array(CARD_SIZE);
  for (let i = 0; i < b.length; i++) b[i] = (i * 31 + (i >> 8) * 7 + seed) & 0xff;
  return b;
}

const same = (a, b) => !!a && !!b && a.length === b.length && a.every((x, i) => x === b[i]);

async function main() {
  const root = await navigator.storage.getDirectory();
  await root.removeEntry("saves", { recursive: true }).catch(() => {});
  const checks = [];
  const check = (name, pass, detail = "") => checks.push({ name, pass, detail });

  check("empty slot exports nothing", (await exportCard(1)) === null);
  const card0 = pattern(1);
  await importCard(0, card0);
  check("import then export is byte-exact", same(await exportCard(0), card0));
  let refused = false;
  try {
    await importCard(1, new Uint8Array(CARD_SIZE - 128));
  } catch {
    refused = true;
  }
  check("a wrong-size image is refused", refused && (await exportCard(1)) === null);

  const card1 = pattern(2);
  const worker = new Worker("storage-worker.js", { type: "module" });
  const reply = await new Promise((res, rej) => {
    worker.onmessage = (e) => res(e.data);
    worker.onerror = (e) => rej(new Error(e.message));
    worker.postMessage({ write: { name: "card1.mcr", data: card1 } });
  });
  if (reply.error) throw new Error(`worker: ${reply.error}`);
  check("the Worker loads the imported card", same(reply.loaded["card0.mcr"], card0));
  check("a Worker save exports byte-exact", same(await exportCard(1), card1));
  check("the saves directory holds both cards", (await Array.fromAsync((await savesDir()).keys())).sort().join() === "card0.mcr,card1.mcr");

  const pass = checks.every((c) => c.pass);
  return { browser, pass, checks };
}

main()
  .catch((e) => ({ browser, pass: false, error: String((e && e.message) || e) }))
  .then(async (result) => {
    out.textContent = JSON.stringify(result, null, 2);
    await fetch(`/result?browser=${browser}`, { method: "POST", body: JSON.stringify(result) });
  });
