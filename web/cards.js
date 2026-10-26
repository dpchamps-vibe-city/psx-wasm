// Memory cards in OPFS (M7): `saves/card0.mcr` and `saves/card1.mcr`, the
// names the runtime's storage_read/storage_write use (psx-bios card.rs).
// Raw 128 KiB images are both the import and the export format, as on the
// native host (psx_devices::memcard). The page imports and exports them;
// the Worker loads every save at start and writes them back as the game
// saves (sync access handles, so `persist` is Worker-only).

export const CARD_SIZE = 0x20000;

export const cardName = (slot) => `card${slot}.mcr`;

export async function savesDir() {
  return (await navigator.storage.getDirectory()).getDirectoryHandle("saves", { create: true });
}

/** Store `bytes` as the card in `slot` (0 or 1). */
export async function importCard(slot, bytes) {
  if (bytes.length !== CARD_SIZE) throw new Error(`a card image is ${CARD_SIZE} bytes, not ${bytes.length}`);
  const w = await (await (await savesDir()).getFileHandle(cardName(slot), { create: true })).createWritable();
  await w.write(bytes);
  await w.close();
}

/** The card in `slot`, or null when there is none yet. */
export async function exportCard(slot) {
  try {
    const h = await (await savesDir()).getFileHandle(cardName(slot));
    return new Uint8Array(await (await h.getFile()).arrayBuffer());
  } catch (e) {
    if (e.name === "NotFoundError") return null;
    throw e;
  }
}

/** Every save file by name. */
export async function loadSaves(dir) {
  const files = new Map();
  for await (const [name, handle] of dir.entries()) {
    if (handle.kind === "file") files.set(name, new Uint8Array(await (await handle.getFile()).arrayBuffer()));
  }
  return files;
}

/** Replace a save file (Worker only). */
export async function persist(dir, name, data) {
  const h = await (await dir.getFileHandle(name, { create: true })).createSyncAccessHandle();
  h.truncate(0);
  h.write(data, { at: 0 });
  h.flush();
  h.close();
}
