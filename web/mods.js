// Mods loaded at run time into a moddable base (plans/M11 phase 2), as the
// native host's loader (crates/psxw-host-native/src/mods.rs) does it:
// each mod module is checked against the base (the game's identity, each
// hooked function's slot and type), given a slice of the mod area and a
// range of synthetic addresses, and wired wasm-to-wasm through the base's
// exported tables, so no JS frame sits between the game and a mod (JSPI
// can still suspend the game). Loading, swapping or removing a mod
// rewires every chain from the mods' order.

const AREA_START = 0x80600000;
const AREA_END = 0x80800000;
const SYNTHETIC_START = 0x80200000;
const SYNTHETIC_END = 0x80400000;

const tableIndex = (a) => ((a & 0x003ffffc) >>> 2);
const text = (buf) => new TextDecoder().decode(buf);
const section = (module, name) => {
  const s = WebAssembly.Module.customSections(module, name);
  return s.length ? text(s[0]) : null;
};
const addr = (s) => parseInt(s, 16) >>> 0;

export class ModLoader {
  /** The loader for a base, or null when game.wasm isn't moddable. */
  static forBase(module, exports) {
    const lines = section(module, "psxmod.slots");
    return lines == null ? null : new ModLoader(lines, section(module, "psxmod.game") || "", exports);
  }

  constructor(lines, identity, exports) {
    this.slots = new Map();
    lines.split("\n").filter(Boolean).forEach((l, i) => {
      const [c, a, ...f] = l.split(" ");
      this.slots.set(`${c} ${addr(a)}`, { slot: i, flavour: f.join(" ") });
    });
    this.identity = identity;
    this.x = exports;
    this.mods = [];
    this.touched = new Set();
    this.dataNext = AREA_START;
    this.codeNext = SYNTHETIC_START;
    this.booted = false;
  }

  slot(container, address) {
    return this.slots.get(`${container} ${addr(address)}`);
  }

  instantiate(bytes, reuse) {
    const module = new WebAssembly.Module(bytes);
    const declText = section(module, "psxmod.decl");
    if (declText == null) throw new Error("not a mod module (no psxmod.decl)");
    const decl = JSON.parse(declText);
    if (section(module, "psxmod.game") !== this.identity) throw new Error(`${decl.id}: built for another game build than this one`);
    const slots = decl.targets.map((t) => {
      const s = this.slot(t.container, t.address);
      if (!s) throw new Error(`${decl.id}: ${t.name} has no hook slot in this base`);
      if (s.flavour !== t.flavour) throw new Error(`${decl.id}: ${t.name} is \`${s.flavour}\` here, the mod was built for \`${t.flavour}\``);
      return s.slot;
    });
    const calls = decl.calls.map((c) => {
      const s = this.slot(c.container, c.address);
      if (!s || s.flavour !== c.flavour) throw new Error(`${decl.id}: calls ${c.name}, which this base doesn't have as \`${c.flavour}\``);
      return s.slot;
    });
    const area = Math.ceil(decl.data_size / 16) * 16 + decl.stack_size;
    let dataBase, codeBase;
    if (reuse && reuse.area >= area && reuse.decl.functions >= decl.functions) {
      ({ dataBase, codeBase } = reuse);
    } else {
      dataBase = this.dataNext;
      if (dataBase + area > AREA_END) throw new Error(`${decl.id}: the mod area is full`);
      this.dataNext += area;
      codeBase = this.codeNext;
      if (codeBase + 4 * decl.functions > SYNTHETIC_END) throw new Error(`${decl.id}: out of synthetic addresses`);
      this.codeNext += 4 * decl.functions;
    }
    const x = this.x;
    const originals = decl.targets.map(() => x.psxmod_chain.grow(1, null));
    const konst = (v) => new WebAssembly.Global({ value: "i32", mutable: false }, v | 0);
    const imports = {
      psx: { memory: x.memory, dispatch: x.psxmod_dispatch, sp: x.__guest_sp },
      psxmod: {
        chain: x.psxmod_chain,
        data_base: konst(dataBase),
        code_index: konst(tableIndex(codeBase)),
        code_base: konst(codeBase),
        stack_top: konst(dataBase + area),
      },
      original: {},
    };
    originals.forEach((idx, k) => (imports.original[k] = konst(idx)));
    for (const imp of WebAssembly.Module.imports(module)) {
      if (imp.module === "psx" && imp.kind === "function") {
        if (!x[imp.name]) throw new Error(`${decl.id}: the base exports no ${imp.name}`);
        imports.psx[imp.name] = x[imp.name];
      }
    }
    const instance = new WebAssembly.Instance(module, imports);
    calls.forEach((slot, k) => instance.exports.psxmod_calls.set(k, x.psxmod_entries.get(slot)));
    return { id: decl.id, decl, instance, slots, originals, dataBase, codeBase, area };
  }

  rewire() {
    const x = this.x;
    const top = new Map();
    for (const m of this.mods) {
      m.slots.forEach((slot, k) => {
        const below = top.has(slot) ? top.get(slot) : x.psxmod_raw.get(slot);
        x.psxmod_chain.set(m.originals[k], below);
        top.set(slot, m.instance.exports[`hook.${k}`]);
        this.touched.add(slot);
      });
    }
    for (const slot of this.touched) x.psxmod_chain.set(slot, top.has(slot) ? top.get(slot) : null);
    this.mods.forEach((m, i) => {
      x.psxmod_on_init.set(i, m.instance.exports.psxmod_init);
      x.psxmod_on_loaded.set(i, m.instance.exports.psxmod_loaded);
    });
    x.psxmod_n_init.value = this.mods.length;
    x.psxmod_n_loaded.value = this.mods.length;
  }

  /** Load a mod after the others. */
  load(bytes) {
    const m = this.instantiate(bytes, null);
    this.mods.push(m);
    this.rewire();
    if (this.booted) m.instance.exports.psxmod_init();
    return m.id;
  }

  /** Replace mod `index`, keeping its state when its data layout is the same. */
  swap(index, bytes) {
    if (index >= this.mods.length) return this.load(bytes);
    const old = this.mods[index];
    const m = this.instantiate(bytes, old);
    const same = m.decl.layout_hash === old.decl.layout_hash && m.dataBase === old.dataBase;
    this.mods[index] = m;
    this.rewire();
    if (this.booted) (same ? m.instance.exports.psxmod_resume : m.instance.exports.psxmod_init)();
    return m.id;
  }

  remove(index) {
    this.mods.splice(index, 1);
    this.rewire();
  }

  ids() {
    return this.mods.map((m) => m.id);
  }
}

// A mailbox for modules pushed while the game runs: the page writes, the
// Worker reads at its next vblank (no event loop needed).
// Words: [seq, index, length]; bytes from offset 12.
export const MAILBOX_BYTES = 8 << 20;

export function postMod(buffer, index, bytes) {
  const words = new Int32Array(buffer, 0, 3);
  if (bytes.length > MAILBOX_BYTES - 12) throw new Error("module too large for the mailbox");
  new Uint8Array(buffer, 12, bytes.length).set(bytes);
  Atomics.store(words, 1, index);
  Atomics.store(words, 2, bytes.length);
  Atomics.add(words, 0, 1);
}

export function takeMod(buffer, seen) {
  const words = new Int32Array(buffer, 0, 3);
  const seq = Atomics.load(words, 0);
  if (seq === seen) return null;
  const len = Atomics.load(words, 2);
  return { seq, index: Atomics.load(words, 1), bytes: new Uint8Array(buffer, 12, len).slice() };
}
