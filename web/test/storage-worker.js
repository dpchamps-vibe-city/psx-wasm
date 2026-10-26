// The Worker half of storage.js: worker.js's save handling (cards.js).

import { loadSaves, persist, savesDir } from "./cards.js";

onmessage = async (e) => {
  try {
    const dir = await savesDir();
    const loaded = Object.fromEntries(await loadSaves(dir));
    const { name, data } = e.data.write;
    await persist(dir, name, data);
    postMessage({ loaded });
  } catch (err) {
    postMessage({ error: String((err && err.message) || err) });
  }
};
