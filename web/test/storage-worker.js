// The Worker half of storage.js: worker.js's save handling. It loads every
// save at start and posts the cards the game changes to the page.

import { loadSaves, savesDir } from "./cards.js";

onmessage = async (e) => {
  try {
    const loaded = Object.fromEntries(await loadSaves(await savesDir()));
    for (const { name, bytes } of e.data.saves) postMessage({ type: "save", name, bytes });
    postMessage({ type: "done", loaded });
  } catch (err) {
    postMessage({ type: "done", error: String((err && err.message) || err) });
  }
};
