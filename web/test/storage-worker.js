// The Worker half of storage.js: it loads every save at start and posts
// the cards the game changes to the page (worker.js's save handling), and
// with { type: "lock" } it opens a bundle's disc through disc.js, holding
// its files' SyncAccessHandles as worker.js does while a game runs.

import { SECTOR, Disc } from "./disc.js";
import { loadSaves, savesDir } from "./cards.js";

let held = null;

/** Raw sector `lba` of the held disc. */
function sector(lba) {
  const out = new Uint8Array(SECTOR);
  return held.readRaw(lba, out) ? out : null;
}

async function saves(pending) {
  const loaded = Object.fromEntries(await loadSaves(await savesDir()));
  for (const { name, bytes } of pending) postMessage({ type: "save", name, bytes });
  postMessage({ type: "done", loaded });
}

onmessage = async (e) => {
  const { data } = e;
  try {
    if (data.type === "lock") {
      const root = await navigator.storage.getDirectory();
      held = await Disc.open(await root.getDirectoryHandle(data.dir), data.cue);
      postMessage({ type: "locked", sectors: held.sectors(), sector: sector(0) });
    } else if (data.type === "read") {
      postMessage({ type: "sector", bytes: sector(data.lba) });
    } else if (data.type === "close") {
      held.close();
      held = null;
      postMessage({ type: "closed" });
    } else {
      await saves(data.saves);
    }
  } catch (err) {
    postMessage({ type: data.type || "done", error: String((err && err.message) || err) });
  }
};
