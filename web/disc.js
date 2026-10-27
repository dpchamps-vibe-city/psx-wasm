// A disc image in OPFS: a .cue sheet over raw 2352-byte .bin files, read
// through FileSystemSyncAccessHandles (Worker only). The same rules as the
// native host's crates/psxw-host-native/src/disc.rs: LBA 0 is the first
// sector of the first file; a track starts at its INDEX 01.

export const SECTOR = 2352;

function msf(s) {
  const p = s.split(":").map((x) => parseInt(x, 10));
  if (p.length !== 3 || p.some(Number.isNaN)) throw new Error(`bad MSF ${s}`);
  return (p[0] * 60 + p[1]) * 75 + p[2];
}

export class Disc {
  constructor(files, tracks) {
    this.files = files; // [{ handle, start, sectors }]
    this.tracks = tracks; // [{ number, data, lba }]
  }

  /** Open `cueName` and the .bin files it names from OPFS directory `dir`. */
  static async open(dir, cueName) {
    const handle = (name) =>
      dir.getFileHandle(name).catch((e) => {
        if (e.name !== "NotFoundError") throw e;
        throw new Error(`${name} is not in this browser's disc storage: import the .cue and every .bin it names again`);
      });
    const cue = await (await (await handle(cueName)).getFile()).text();
    const files = [];
    const tracks = [];
    let cur = null;
    for (const raw of cue.split(/\r?\n/)) {
      const line = raw.trim();
      if (line.startsWith("FILE ")) {
        const rest = line.slice(5);
        const cut = rest.lastIndexOf(" ");
        const name = (cut > 0 ? rest.slice(0, cut) : rest).replace(/^"|"$/g, "");
        const sync = await (await handle(name)).createSyncAccessHandle();
        const len = sync.getSize();
        if (len % SECTOR !== 0) throw new Error(`${name} is not raw 2352-byte sectors`);
        const last = files[files.length - 1];
        files.push({ handle: sync, start: last ? last.start + last.sectors : 0, sectors: len / SECTOR });
      } else if (line.startsWith("TRACK ")) {
        const [n, kind] = line.slice(6).trim().split(/\s+/);
        if (!["AUDIO", "MODE2/2352", "MODE1/2352"].includes(kind)) throw new Error(`track ${n}: unsupported type ${kind}`);
        cur = { number: parseInt(n, 10), data: kind !== "AUDIO" };
      } else if (line.startsWith("INDEX 01 ")) {
        if (!cur || files.length === 0) throw new Error("INDEX before TRACK or FILE");
        tracks.push({ ...cur, lba: files[files.length - 1].start + msf(line.slice(9).trim()) });
      }
    }
    if (files.length === 0 || tracks.length === 0) throw new Error(`${cueName} names no tracks`);
    return new Disc(files, tracks);
  }

  sectors() {
    const f = this.files[this.files.length - 1];
    return f.start + f.sectors;
  }

  /** psxw_abi::DiscInfo: tracks, sectors, track_lba[99], track_data[99]. */
  info(view, ptr) {
    view.setUint32(ptr, this.tracks.length, true);
    view.setUint32(ptr + 4, this.sectors(), true);
    this.tracks.slice(0, 99).forEach((t, i) => {
      view.setUint32(ptr + 8 + 4 * i, t.lba, true);
      view.setUint8(ptr + 8 + 4 * 99 + i, t.data ? 1 : 0);
    });
  }

  /** One raw sector into `out` (a Uint8Array of 2352); false if absent. */
  readRaw(lba, out) {
    const f = this.files.find((f) => lba >= f.start && lba < f.start + f.sectors);
    if (!f) return false;
    return f.handle.read(out, { at: (lba - f.start) * SECTOR }) === SECTOR;
  }

  close() {
    for (const f of this.files) f.handle.close();
  }
}

/** The 2048 user bytes of a raw sector (mode 1 or mode 2 form 1). */
export function userData(raw) {
  return raw[15] === 1 ? raw.subarray(0x10, 0x810) : raw.subarray(0x18, 0x818);
}
