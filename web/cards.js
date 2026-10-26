// Memory cards in OPFS (M7): `saves/card0.mcr` and `saves/card1.mcr`, the
// names the runtime's storage_read/storage_write use (psx-bios card.rs).
// Raw 128 KiB images are both the import and the export format, as on the
// native host (psx_devices::memcard). The page imports and exports them;
// the Worker loads every save at start and posts each card the game
// changes to the page, whose SaveQueue writes it: the game seldom lets the
// Worker's event loop run, and OPFS promises resolve only there.

export const CARD_SIZE = 0x20000;

export const cardName = (slot) => `card${slot}.mcr`;

export async function savesDir() {
  return (await navigator.storage.getDirectory()).getDirectoryHandle("saves", { create: true });
}

/** Replace the save file `name` (written whole, then swapped in). */
async function writeSave(name, bytes) {
  const w = await (await (await savesDir()).getFileHandle(name, { create: true })).createWritable();
  await w.write(bytes);
  await w.close();
}

/** Store `bytes` as the card in `slot` (0 or 1). */
export async function importCard(slot, bytes) {
  if (bytes.length !== CARD_SIZE) throw new Error(`a card image is ${CARD_SIZE} bytes, not ${bytes.length}`);
  await writeSave(cardName(slot), bytes);
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

/**
 * The saves a running game makes, written one at a time. The game rewrites
 * a card every vblank while it saves, so a file's queued write is replaced
 * by a newer one rather than queued behind it.
 */
export class SaveQueue {
  constructor(onError) {
    this.onError = onError;
    this.queued = new Map();
    this.latest = new Map();
    this.writing = null;
  }

  put(name, bytes) {
    this.queued.set(name, bytes);
    this.latest.set(name, bytes);
    this.writing ||= this.drain();
  }

  async drain() {
    while (this.queued.size) {
      const [name, bytes] = this.queued.entries().next().value;
      this.queued.delete(name);
      try {
        await writeSave(name, bytes);
      } catch (e) {
        this.onError(name, e);
      }
    }
    this.writing = null;
  }

  /** Resolves once everything put so far is written. */
  async idle() {
    while (this.writing) await this.writing;
  }
}
