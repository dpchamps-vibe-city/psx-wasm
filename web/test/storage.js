// Memory cards through OPFS (M7): the page's import and export
// (cards.js) round-trip an image byte for byte and refuse a wrong size;
// the Worker's side (storage-worker.js, as worker.js does it) loads what
// the page imported and posts saves, which the page's SaveQueue writes
// and then exports.
//
// Discs through OPFS (M5/M7): two bundles on one origin (test-a/,
// test-b/), each the real page (index.html, main.js) with its own game.json
// id, import the same file names through ?discurl into their own OPFS
// directory and localStorage key. A reimport of different bytes while a
// Worker holds the disc's SyncAccessHandles open (as a running game does)
// must be refused with the running-game diagnostic and leave the imported
// cue and .bin intact; after that Worker closes, the same reimport must
// succeed without touching bundle test-b.
//
// The result is POSTed to /result?browser=NAME and shown on the page.

import { CARD_SIZE, SaveQueue, exportCard, importCard, savesDir } from "./cards.js";

const browser = new URLSearchParams(location.search).get("browser") || "unknown";
const out = document.getElementById("out");

/** A card-sized image whose every byte depends on its offset and `seed`. */
function pattern(seed) {
  const b = new Uint8Array(CARD_SIZE);
  for (let i = 0; i < b.length; i++) b[i] = (i * 31 + (i >> 8) * 7 + seed) & 0xff;
  return b;
}

const same = (a, b) => !!a && !!b && a.length === b.length && a.every((x, i) => x === b[i]);

/** `src` in an iframe on this page, once it has loaded. */
function frame(src) {
  return new Promise((res, rej) => {
    const f = document.createElement("iframe");
    f.src = src;
    f.onload = () => res(f);
    f.onerror = () => rej(new Error(`cannot load ${src}`));
    document.body.append(f);
  });
}

const status = (f) => f.contentDocument.getElementById("status").textContent;
const ready = (f) => !f.contentDocument.getElementById("start").disabled;

/** `pred` within 20 s, so a page that never gets there fails its check. */
async function eventually(pred) {
  for (let i = 0; i < 200; i++) {
    if (await pred()) return true;
    await new Promise((r) => setTimeout(r, 100));
  }
  return pred();
}

/** The bytes of `name` in bundle `id`'s OPFS disc directory, or null. */
async function discFile(root, id, name) {
  try {
    const dir = await root.getDirectoryHandle(`disc-${id}`);
    return new Uint8Array(await (await (await dir.getFileHandle(name)).getFile()).arrayBuffer());
  } catch (e) {
    if (e.name === "NotFoundError") return null;
    throw e;
  }
}

/** The text of `name` in bundle `id`'s OPFS disc directory, or null. */
async function discText(root, id, name) {
  const bytes = await discFile(root, id, name);
  return bytes && new TextDecoder().decode(bytes);
}

/** Whether `name` in bundle `id` can be rewritten, i.e. no Worker holds it. */
async function writable(root, id, name) {
  try {
    const dir = await root.getDirectoryHandle(`disc-${id}`);
    const w = await (await dir.getFileHandle(name)).createWritable();
    await w.abort();
    return true;
  } catch (e) {
    if (e.name === "NoModificationAllowedError") return false;
    throw e;
  }
}

async function main() {
  const root = await navigator.storage.getDirectory();
  await root.removeEntry("saves", { recursive: true }).catch(() => {});
  for (const id of ["test-a", "test-b"]) {
    localStorage.removeItem(`psxw.cue.${id}`);
    await root.removeEntry(`disc-${id}`, { recursive: true }).catch(() => {});
  }
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

  // Two saves in a row, as a game makes them: the last one wins.
  const card1 = pattern(2);
  const failed = [];
  const queue = new SaveQueue((name, e) => failed.push(`${name}: ${e}`));
  const worker = new Worker("storage-worker.js", { type: "module" });
  /** Post `msg` and take the Worker's reply (its saves go to the queue). */
  const ask = (msg) =>
    new Promise((res, rej) => {
      worker.onmessage = (e) => (e.data.type === "save" ? queue.put(e.data.name, e.data.bytes) : res(e.data));
      worker.onerror = (e) => rej(new Error(e.message));
      worker.postMessage(msg);
    });
  const reply = await ask({ saves: [{ name: "card1.mcr", bytes: pattern(3) }, { name: "card1.mcr", bytes: card1 }] });
  if (reply.error) throw new Error(`worker: ${reply.error}`);
  await queue.idle();
  check("the Worker loads the imported card", same(reply.loaded["card0.mcr"], card0));
  check("every save is written", failed.length === 0, failed.join("; "));
  check("the last of a Worker's saves exports byte-exact", same(await exportCard(1), card1));
  check("the saves directory holds both cards", (await Array.fromAsync((await savesDir()).keys())).sort().join() === "card0.mcr,card1.mcr");

  // The disc lane. Both bundles name their files test.cue and track01.bin;
  // only their bytes differ.
  const bins = {};
  const cues = {};
  for (const id of ["test-a", "test-b"]) {
    bins[id] = new Uint8Array(await (await fetch(`${id}/disc/track01.bin`)).arrayBuffer());
    cues[id] = await (await fetch(`${id}/disc/test.cue`)).text();
  }
  const pages = {};
  for (const id of ["test-a", "test-b"]) {
    pages[id] = await frame(`${id}/index.html?discurl=disc/test.cue`);
    const ok = await eventually(() => ready(pages[id]));
    check(`bundle ${id} imports its disc through the page`, ok, status(pages[id]));
    check(
      `bundle ${id} keeps its disc under its own id`,
      same(await discFile(root, id, "track01.bin"), bins[id]) && (await discText(root, id, "test.cue")) === cues[id] && localStorage.getItem(`psxw.cue.${id}`) === "test.cue",
      status(pages[id]),
    );
  }
  check("importing the second bundle leaves the first disc alone", same(await discFile(root, "test-a", "track01.bin"), bins["test-a"]));
  check("the two bundles' discs differ", !same(bins["test-a"], bins["test-b"]));

  // Different bytes at the same file names: the import must not replace any
  // of the old image unless all of it can be replaced.
  const replacedCue = await (await fetch("test-a/replacement/test.cue")).text();
  const replacedBin = new Uint8Array(await (await fetch("test-a/replacement/track01.bin")).arrayBuffer());
  check("the replacement differs from the imported disc", !same(replacedBin, bins["test-a"]) && replacedCue !== cues["test-a"]);

  // A running game's Worker holds the disc's sectors open; a reimport must
  // be refused rather than write over it.
  const locked = await ask({ type: "lock", dir: "disc-test-a", cue: "test.cue" });
  if (locked.error) throw new Error(`worker: ${locked.error}`);
  check("a Worker holds the disc open", locked.sectors === 4 && same(locked.sector, bins["test-a"].subarray(0, 2352)), `${locked.sectors}`);
  const again = await frame("test-a/index.html?discurl=replacement/test.cue");
  const diagnosed = await eventually(() => /is open in a running game/.test(status(again)));
  check("a reimport over a locked disc is refused", diagnosed, status(again));
  const reread = await ask({ type: "read", lba: 0 });
  check("the locked disc is not overwritten", same(await discFile(root, "test-a", "track01.bin"), bins["test-a"]) && same(reread.bytes, locked.sector), status(again));
  check("the locked cue is not overwritten", (await discText(root, "test-a", "test.cue")) === cues["test-a"], status(again));
  check("the imported disc is still remembered", localStorage.getItem("psxw.cue.test-a") === "test.cue");

  const closed = await ask({ type: "close" });
  if (closed.error) throw new Error(`worker: ${closed.error}`);
  check("the Worker releases the disc", await eventually(() => writable(root, "test-a", "track01.bin")));
  const reimported = await frame("test-a/index.html?discurl=replacement/test.cue");
  check("after the Worker closes, the replacement imports", await eventually(() => ready(reimported)), status(reimported));
  check(
    "the replacement is stored",
    same(await discFile(root, "test-a", "track01.bin"), replacedBin) && (await discText(root, "test-a", "test.cue")) === replacedCue,
    status(reimported),
  );
  check(
    "bundle test-b is unchanged by the replacement",
    same(await discFile(root, "test-b", "track01.bin"), bins["test-b"]) &&
      (await discText(root, "test-b", "test.cue")) === cues["test-b"] &&
      localStorage.getItem("psxw.cue.test-b") === "test.cue",
  );
  worker.terminate();

  const pass = checks.every((c) => c.pass);
  return { browser, pass, checks };
}

main()
  .catch((e) => ({ browser, pass: false, error: String((e && e.message) || e) }))
  .then(async (result) => {
    out.textContent = JSON.stringify(result, null, 2);
    await fetch(`/result?browser=${browser}`, { method: "POST", body: JSON.stringify(result) });
  });
