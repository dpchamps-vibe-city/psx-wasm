// Script mods (plans/M12, D54): a mod is one ES module whose default export,
// `mod(factory)`, returns its definition: hooks, data patches and
// commands, written against the mod's `game` (names from a snapshot,
// D55) and the mod API. No build step: the loader writes a wasm shim for
// the mod (shims.js), which joins each hooked slot's chain as a layer
// beside compiled mods' (mods.js), in load order.
//
// - Hooks: `before`, `after`, `replace` and `around` (with `original`, the
//   next link). JS runs inside the game: the shim brackets each call with
//   the runtime's `psxw_js_enter`/`psxw_js_leave`, so the host's vblank
//   wait is held while JS is on the stack (ABI 10).
// - Game calls (`game.F(...)`) work only inside the game: in a hook, a
//   command, `init` or `psx.run`. Memory reads and writes work anywhere.
// - Commands and `psx.run` callbacks run inside the game through the
//   runtime's queued call (D52), at synthetic addresses.
// - A handler that throws is logged; the call it was handling passes to
//   the next link, and its mod is disabled at the next vblank.
// - Mods are installed, reloaded and removed at a vblank (`vblank()`),
//   never while game code runs.

import { calls as callsModule, layer, slotType } from "./shims.js";
import { Types } from "./types.js";

const MOD = Symbol.for("psxw.mod");
const TARGET = Symbol.for("psxw.target");
const HOOK = Symbol.for("psxw.hook");
const RECORD = Symbol.for("psxw.record");
const KINDS = ["before", "after", "replace", "around"];
const SCALARS = {
  u8: [1, "getUint8", "setUint8"],
  s8: [1, "getInt8", "setInt8"],
  u16: [2, "getUint16", "setUint16"],
  s16: [2, "getInt16", "setInt16"],
  u32: [4, "getUint32", "setUint32"],
  s32: [4, "getInt32", "setInt32"],
};
const hex = (a) => "0x" + (a >>> 0).toString(16).toUpperCase().padStart(8, "0");
const describe = (e) => (e && e.stack) || String(e);

/** A script mod's default export: `export default mod(({ game, hook }) => ({ … }))`. */
export function mod(factory) {
  if (typeof factory !== "function") throw new TypeError("mod(factory): give it a function that returns the mod's definition");
  return { [MOD]: factory };
}

/** splat's names for what no symbol file names: `func_ADDR`, `<container>_func_ADDR`, `D_ADDR`. */
function autoName(name) {
  const m = /^([A-Za-z_][A-Za-z0-9_]*)_([0-9A-Fa-f]{8})$/.exec(name);
  if (!m) return null;
  const address = parseInt(m[2], 16) >>> 0;
  if (address < 0x80000000 || address >= 0x80200000) return null;
  const func = m[1] === "func" || m[1].endsWith("_func");
  return { address, func, container: m[1].endsWith("_func") ? m[1].slice(0, -5) : null };
}

/**
 * The queued calls into the game (D52), one at a time: `psxw_execute`
 * takes one; the next is queued when its result comes back (`done`).
 */
export class Calls {
  constructor(exports) {
    this.x = exports;
    this.queue = [];
    this.inflight = null;
    this.nextId = 1;
  }

  /** Queue a call of the o32 function at `address`; resolves with `{ value, js }` once it has run. */
  enqueue(address, args = [], js = null) {
    return new Promise((resolve, reject) => {
      const words = args.map((a) => a >>> 0);
      if (words.length > 4) throw new Error("a queued call takes up to four argument words");
      this.queue.push({ id: this.nextId++, address: address >>> 0, args: words, js, resolve, reject });
      this.pump();
    });
  }

  pump() {
    while (!this.inflight && this.queue.length) {
      const c = this.queue[0];
      const a = (i) => c.args[i] ?? 0;
      const status = this.x.psxw_execute(c.id, c.address, a(0), a(1), a(2), a(3));
      if (status === -2) return; // the runtime is busy; retry at the next vblank
      this.queue.shift();
      if (status === 0) this.inflight = c;
      else c.reject(new Error(`call rejected: ${status === -3 ? `invalid address ${hex(c.address)}` : "runtime not ready"}`));
    }
  }

  /** The runtime's `execute` event: whether it was one of ours. */
  done(id, value) {
    const c = this.inflight;
    if (!c || c.id !== id) return false;
    this.inflight = null;
    c.resolve({ value: BigInt(value), js: c.js });
    this.pump();
    return true;
  }

  current() {
    return this.inflight;
  }

  fail(reason) {
    for (const c of [this.inflight, ...this.queue]) c?.reject(new Error(reason));
    this.inflight = null;
    this.queue = [];
  }
}

/** A snapshot's indexes and types, for its `game`s. */
class Names {
  constructor(snapshot, mem, window) {
    this.snapshot = snapshot;
    this.types = new Types(snapshot, mem, window);
    this.main = snapshot.main;
    this.containers = new Map(snapshot.containers.map((c) => [c.id, c]));
    this.functions = new Map();
    this.globals = new Map();
    const add = (map, e) => {
      for (const n of [...e.names, ...(e.deprecated || [])]) {
        const list = map.get(n) || [];
        if (!list.includes(e)) list.push(e);
        map.set(n, list);
      }
    };
    for (const f of snapshot.functions) add(this.functions, f);
    for (const g of snapshot.globals) add(this.globals, g);
    this.byAddress = new Map();
    for (const f of snapshot.functions) this.byAddress.set(`${f.container} ${f.address}`, f);
  }

  /** The one function named `name` (in `container`, if given). */
  func(name, container) {
    let list = this.functions.get(name);
    const auto = !list && autoName(name);
    if (auto && auto.func) {
      const c = container || auto.container || this.main;
      const f = this.byAddress.get(`${c} ${auto.address}`);
      return f || { container: c, address: auto.address, names: [name] };
    }
    if (!list) return null;
    if (container) list = list.filter((f) => f.container === container);
    if (list.length > 1) {
      const where = list.map((f) => `${f.container} ${hex(f.address)}`).join(", ");
      throw new Error(`${name} names more than one function (${where}): use game.at(container, name)`);
    }
    return list[0] || null;
  }

  /** The one global named `name` (in `container`, if given): `{ container, address, size, type }`. */
  global(name, container) {
    let list = this.globals.get(name);
    if (!list) {
      const auto = autoName(name);
      if (!auto || auto.func) return null;
      return { container: container || this.containerOf(auto.address), address: auto.address, names: [name] };
    }
    if (container) list = list.filter((g) => g.container === container);
    if (list.length > 1) {
      const where = list.map((g) => `${g.container} ${hex(g.address)}`).join(", ");
      throw new Error(`${name} names more than one global (${where}): use game.at(container, name)`);
    }
    return list[0] || null;
  }

  /** The container that loads `address`: the executable's, else the one overlay, else null. */
  containerOf(address) {
    const main = this.containers.get(this.main);
    if (main && address >= main.loaded[0] && address < main.loaded[1]) return this.main;
    const at = this.snapshot.globals.filter((g) => g.address === address).map((g) => g.container);
    const unique = [...new Set(at)];
    if (unique.length === 1) return unique[0];
    const holding = [...this.containers.values()].filter((c) => address >= c.loaded[0] && address < c.loaded[1]);
    return holding.length === 1 ? holding[0].id : null;
  }
}

/** Script mods in a moddable base (the Worker has one). */
export class ScriptHost {
  /**
   * `exports`: game.wasm's; `loader`: the base's ModLoader; `snapshot`: the
   * bundle's (snapshot.json); `calls`: the queued calls; `log(text)`: a
   * line for the page and the run's logs; `status(text)`: for the page.
   */
  constructor({ exports, loader, snapshot, windowBase, calls, log, status }) {
    this.x = exports;
    this.loader = loader;
    this.window = windowBase;
    this.calls = calls;
    this.logLine = log;
    this.status = status;
    this.hi = new WebAssembly.Global({ value: "i32", mutable: true }, 0);
    // Shims calling JS (hooks, commands, init); host calls (vblank) inside.
    this.inside = 0;
    this.hostDepth = 0;
    // Errors that came out of game code, which handlers pass on.
    this.fromGame = new WeakSet();
    this.dv = () => new DataView(this.x.memory.buffer);
    this.names = new Names(snapshot, this.dv, windowBase);
    this.snapshots = new Map([[snapshot.source?.version ?? "bundle", this.names]]);
    this.records = new Map();
    this.pending = [];
    this.consoleN = 0;
    this.resident = loader.resident;
    const b = { enter: exports.psxw_js_enter, leave: exports.psxw_js_leave, hi: this.hi };
    const flavours = [...new Set([...loader.slots.values()].map((s) => s.flavour))];
    this.helpers = new WebAssembly.Instance(new WebAssembly.Module(callsModule(flavours)), {
      b: { ...b, entries: exports.psxmod_entries, dispatch: exports.psxmod_dispatch },
      j: { run: (a0, a1, a2, a3) => this.runQueued(a0, a1, a2, a3) },
    }).exports;
    this.runAddress = loader.synthetic(1);
    exports.psxmod_dispatch.set(this.loader.tableIndex(this.runAddress), this.helpers.run);
    this.game = this.makeGame(this.names);
    this.names.game = this.game;
    this.mem = this.makeMem();
  }

  // --- inside the game -------------------------------------------------

  /** Whether game code may be called now: JS was called by the game, not by the host. */
  inGame() {
    return this.inside > 0 && this.hostDepth === 0;
  }

  /** Run host work (a vblank's) with game calls refused. */
  asHost(fn) {
    this.hostDepth++;
    try {
      return fn();
    } finally {
      this.hostDepth--;
    }
  }

  /** Call game code; errors from it are the game's, passed through handlers. */
  intoGame(fn) {
    try {
      return fn();
    } catch (e) {
      if (e && typeof e === "object") this.fromGame.add(e);
      throw e;
    }
  }

  /** A handler threw: the game's error goes on; the mod's is logged and the mod disabled. */
  fault(m, what, e) {
    if (e && typeof e === "object" && this.fromGame.has(e)) throw e;
    this.logLine(`mod ${m.id}: ${what} threw: ${describe(e)}; ${m.id} is disabled at the next vblank`);
    if (!m.disabling) {
      m.disabling = true;
      this.pending.push({ op: "disable", id: m.id });
    }
  }

  /** Words for an o32 call: four in registers, the rest on the guest stack. */
  o32Call(address, words) {
    const x = this.x;
    const sp = x.__guest_sp.value;
    const frame = Math.ceil((Math.max(words.length, 4) * 4) / 8) * 8;
    const newSp = (sp - frame) | 0;
    if (words.length > 4) {
      const dv = new DataView(x.memory.buffer);
      for (let k = 4; k < words.length; k++) dv.setUint32(this.window + ((newSp + 4 * k) & 0xffffff), words[k] >>> 0, true);
    }
    x.__guest_sp.value = newSp;
    try {
      const v0 = this.helpers.o32(address, words[0] | 0, words[1] | 0, words[2] | 0, words[3] | 0);
      return [v0, this.hi.value];
    } finally {
      x.__guest_sp.value = sp;
    }
  }

  /** Call a game function by its target: the head of its chain. */
  callGame(t, args) {
    if (!this.inGame()) {
      throw new Error(`${t.name}: game functions run only inside the game (in a hook, a command, init, or psx.run(() => …))`);
    }
    const ty = t.flavour ? slotType(t.flavour) : slotType("o32");
    if (t.typed === undefined) t.typed = (t.sig && t.types.signature(t.sig, ty, this.hi, () => this.x.__guest_sp.value)) || null;
    const typed = t.typed;
    if (typed) {
      return this.intoGame(() => {
        if (t.slot !== undefined && !ty.o32) return typed.value(this.helpers[`c:${t.flavour}`](t.slot, ...typed.words(args)));
        const [v0] = this.o32Call(t.address, typed.list(args));
        return typed.value(v0);
      });
    }
    return this.intoGame(() => {
      if (t.slot !== undefined && t.flavour !== "o32") {
        const words = [];
        ty.params.forEach((p, i) => {
          const v = args[i] ?? 0;
          if (p === "i64") {
            const b = BigInt.asUintN(64, BigInt(v));
            words.push(Number(b & 0xffffffffn) | 0, Number(b >> 32n) | 0);
          } else words.push(v);
        });
        const r = this.helpers[`c:${t.flavour}`](t.slot, ...words);
        if (ty.results[0] === "i64") return BigInt.asIntN(64, (BigInt(this.hi.value >>> 0) << 32n) | BigInt(r >>> 0));
        return ty.results.length ? r : undefined;
      }
      const [v0] = this.o32Call(t.address, args.map((a) => Number(a) >>> 0));
      return v0;
    });
  }

  /** The `run` entry: the queued `psx.run` callback, inside the game. */
  runQueued() {
    const c = this.calls.current();
    this.inside++;
    try {
      if (!c || !c.js || !c.js.run) return 0;
      const r = c.js.run();
      if (r && typeof r.then === "function") throw new Error("psx.run callbacks run inside the game, synchronously: don't return a Promise");
      c.js.result = r;
      return word(r, this.hi);
    } catch (e) {
      if (e && typeof e === "object" && this.fromGame.has(e)) throw e;
      if (c && c.js) c.js.error = e;
      return 0;
    } finally {
      this.inside--;
    }
  }

  // --- what a mod sees --------------------------------------------------

  makeMem() {
    const w = this.window;
    const dv = () => new DataView(this.x.memory.buffer);
    const m = {};
    for (const [t, [, get, set]] of Object.entries(SCALARS)) {
      m[t] = (a) => dv()[get](w + (a & 0xffffff), true);
      m[`set${t[0].toUpperCase()}${t.slice(1)}`] = (a, v) => dv()[set](w + (a & 0xffffff), Number(v), true);
    }
    m.bytes = (a, n) => new Uint8Array(this.x.memory.buffer, w + (a & 0xffffff), n).slice();
    m.setBytes = (a, bytes) => new Uint8Array(this.x.memory.buffer, w + (a & 0xffffff), bytes.length).set(bytes);
    return Object.freeze(m);
  }

  /** A record type of the mod's own: `psx.struct({ hp: { type: "s16", offset: 0x40 } })`. */
  struct(fields, name = "struct") {
    const mem = this.mem;
    const proto = {};
    for (const [f, d] of Object.entries(fields)) {
      const spec = typeof d === "string" ? { type: d } : d;
      if (!SCALARS[spec.type]) throw new Error(`${name}.${f}: type ${spec.type} isn't one of ${Object.keys(SCALARS).join(", ")}`);
      const off = spec.offset ?? 0;
      const set = `set${spec.type[0].toUpperCase()}${spec.type.slice(1)}`;
      Object.defineProperty(proto, f, {
        get() {
          return mem[spec.type](this[RECORD] + off);
        },
        set(v) {
          mem[set](this[RECORD] + off, v);
        },
        enumerable: true,
      });
    }
    return Object.freeze({
      fields,
      at(address) {
        const v = Object.create(proto);
        v[RECORD] = address >>> 0;
        return v;
      },
    });
  }

  /** A target for a function: callable, and a hook target. */
  gameFn(f, names) {
    const s = this.loader.slot(f.container, f.address);
    const t = {
      kind: "fn",
      name: f.names[0],
      container: f.container,
      address: f.address >>> 0,
      slot: s?.slot,
      flavour: s?.flavour,
      tier: s?.tier,
      snapshot: names.snapshot,
      sig: f.signature || null,
      types: names.types,
    };
    const call = (...args) => this.callGame(t, args);
    Object.defineProperties(call, {
      [TARGET]: { value: t },
      address: { value: t.address, enumerable: true },
      container: { value: t.container, enumerable: true },
      toString: { value: () => `${t.name} (${t.container} ${hex(t.address)})` },
    });
    return call;
  }

  /** A mod's `game`, from a snapshot: functions, `addr`, `points` and `at`. */
  makeGame(names) {
    const fns = new Map();
    const fn = (f) => {
      const key = `${f.container} ${f.address}`;
      if (!fns.has(key)) fns.set(key, this.gameFn(f, names));
      return fns.get(key);
    };
    const unknown = (what, name) => new Error(`${what} ${name}: no such name in the snapshot (${names.snapshot.source?.kind ?? "?"} ${names.snapshot.source?.version ?? ""})`);
    // Names not defined up front (auto-names, misspellings) land here.
    const missing = (resolve) =>
      new Proxy(Object.create(null), {
        get(_, key) {
          if (typeof key === "symbol" || key === "then" || key === "toJSON") return undefined;
          if (key in Object.prototype) return Object.prototype[key];
          return resolve(key);
        },
      });
    const addr = Object.create(
      missing((key) => {
        const g = names.global(key) || names.func(key);
        if (!g) throw unknown("game.addr", key);
        return g.address >>> 0;
      }),
    );
    for (const [n, list] of names.globals) if (list.length === 1) addr[n] = list[0].address >>> 0;
    const points = Object.create(
      missing((key) => {
        throw new Error(`no hook point ${key} (the game's manifest names: ${Object.keys(names.snapshot.points || {}).join(", ") || "none"})`);
      }),
    );
    for (const [p, spec] of Object.entries(names.snapshot.points || {})) {
      const kind = spec.before ? "before" : "after";
      const of = spec.before || spec.after;
      Object.defineProperty(points, p, {
        enumerable: true,
        get() {
          const f = names.func(of);
          if (!f) throw new Error(`hook point ${p} is ${kind} ${of}, which isn't in the snapshot`);
          return { [TARGET]: { kind: "point", name: p, point: kind, fn: fn(f)[TARGET] }, toString: () => `point ${p} (${kind} ${of})` };
        },
      });
    }
    const game = Object.create(
      missing((key) => {
        const f = names.func(key);
        if (f) return fn(f);
        throw unknown("game", key);
      }),
    );
    const types = names.types;
    Object.defineProperties(game, {
      addr: { value: Object.freeze(addr), enumerable: true },
      points: { value: Object.freeze(points), enumerable: true },
      snapshot: { value: names.snapshot.source, enumerable: true },
      types: { enumerable: true, get: () => types.records() },
      at: {
        enumerable: true,
        /** A function by container and address (or name in that container). */
        value(container, where) {
          if (typeof where === "string") {
            const f = names.func(where, container);
            if (!f) throw new Error(`no function ${where} in ${container}`);
            return fn(f);
          }
          const a = where >>> 0;
          return fn(names.byAddress.get(`${container} ${a}`) || { container, address: a, names: [`${container}:${hex(a)}`] });
        },
      },
    });
    // Typed globals: scalars as values, arrays and records as views.
    for (const [n, list] of names.globals) {
      if (list.length !== 1 || !list[0].type || n in game) continue;
      const g = list[0];
      const a = g.address >>> 0;
      Object.defineProperty(game, n, {
        enumerable: true,
        get: () => types.get(g.type, a),
        set: (v) => types.set(g.type, a, v, n),
      });
    }
    for (const [n, list] of names.functions) {
      if (list.length !== 1 || n in game) continue;
      Object.defineProperty(game, n, {
        enumerable: true,
        configurable: true,
        get() {
          const v = fn(list[0]);
          Object.defineProperty(game, n, { value: v, enumerable: true });
          return v;
        },
      });
    }
    return game;
  }

  /** The API a mod's factory gets. */
  api(m) {
    return Object.freeze({
      game: m.game,
      mem: this.mem,
      hook: (target, handlers) => this.hookDecl(target, handlers),
      log: (...args) => this.logLine(`mod: ${m.id}: ${args.map((a) => (typeof a === "string" ? a : JSON.stringify(a))).join(" ")}`),
      psx: this.console(),
    });
  }

  hookDecl(target, handlers) {
    const t = target && target[TARGET];
    if (!t) throw new TypeError(`hook(target, handlers): ${String(target)} isn't a hook target (game.Name, game.points.name, game.at(container, address))`);
    if (!handlers || typeof handlers !== "object") throw new TypeError(`hook(${t.name}, handlers): handlers is an object of before, after, replace or around`);
    const kinds = Object.keys(handlers);
    const bad = kinds.filter((k) => !KINDS.includes(k) || typeof handlers[k] !== "function");
    if (bad.length) throw new TypeError(`hook(${t.name}): ${bad.join(", ")}: handlers are functions named ${KINDS.join(", ")}`);
    if (!kinds.length) throw new TypeError(`hook(${t.name}): no handlers`);
    if (handlers.replace && handlers.around) throw new TypeError(`hook(${t.name}): replace and around both take the call; give one`);
    return { [HOOK]: true, target: t, handlers };
  }

  // --- loading -------------------------------------------------------------

  /**
   * Import a script mod's module; its factory runs when it is installed.
   * A `psxmod.json` beside it ({ "snapshot": URL }) names the snapshot its
   * `game` comes from (D55); else the bundle's.
   */
  async importMod(url, as) {
    const module = await import(url);
    const d = module.default;
    if (!d || !d[MOD]) throw new Error(`${url}: its default export isn't mod(factory)`);
    let names = null;
    let meta = null;
    try {
      const r = await fetch(new URL("psxmod.json", url), { cache: "no-store" });
      if (r.ok) meta = await r.json();
    } catch {}
    if (meta && meta.snapshot) names = await this.snapshotAt(new URL(meta.snapshot, url).href);
    return { url, factory: d[MOD], as, names };
  }

  /**
   * A packed mod (`psxw mod pack`): its module, imported from a blob, and
   * the part of its snapshot it uses, which must describe this binary.
   */
  async importPack(pack) {
    if (!pack || pack.psxpack !== 1 || typeof pack.mod !== "string" || !pack.snapshot) throw new Error("not a mod pack (psxw mod pack)");
    if (JSON.stringify(pack.snapshot.game) !== JSON.stringify(this.names.snapshot.game)) {
      throw new Error(`${pack.id}: packed for another game binary than this bundle's`);
    }
    const url = URL.createObjectURL(new Blob([pack.mod, `\n//# sourceURL=psxw-mods/${pack.id}.js\n`], { type: "text/javascript" }));
    const module = await import(url);
    const d = module.default;
    if (!d || !d[MOD]) throw new Error(`${pack.id}: its default export isn't mod(factory)`);
    return { url, factory: d[MOD], names: new Names(pack.snapshot, this.dv, this.window) };
  }

  /** A snapshot by URL, read once. */
  async snapshotAt(url) {
    if (!this.snapshots.has(url)) {
      const r = await fetch(url);
      if (!r.ok) throw new Error(`${url}: ${r.status}`);
      this.snapshots.set(url, new Names(await r.json(), this.dv, this.window));
    }
    return this.snapshots.get(url);
  }

  /** A snapshot's `game` (one per snapshot). */
  gameOf(names) {
    if (!names.game) names.game = names === this.names ? this.game : this.makeGame(names);
    return names.game;
  }

  /** Install an imported mod at the next vblank: `op` load, reload or swap at `index`. */
  queue(item) {
    return new Promise((resolve, reject) => this.pending.push({ op: "install", ...item, resolve, reject }));
  }

  /** At a vblank (and before boot): apply what's pending. Returns what happened. */
  vblank() {
    const what = [];
    while (this.pending.length) {
      const p = this.pending.shift();
      try {
        if (p.op === "install") {
          const id = this.install(p);
          what.push(`${p.replaced ? "reloaded" : "loaded"} ${id}`);
          p.resolve?.(id);
        } else if (p.op === "disable" || p.op === "unload") {
          const i = this.loader.mods.findIndex((m) => m.id === p.id && m.kind === "script");
          if (i >= 0) {
            this.loader.remove(i);
            what.push(`${p.op === "disable" ? "disabled" : "unloaded"} ${p.id}`);
          }
          p.resolve?.();
        }
      } catch (e) {
        what.push(`failed: ${e.message || e}`);
        this.logLine(`mod load failed: ${describe(e)}`);
        p.reject?.(e);
        // A run's own mods (its options) must load.
        if (p.fatal) throw e;
      }
    }
    return what;
  }

  /** Run a mod's factory, check its definition, build its layer and wire it in. */
  install({ url, factory, as, index, names }) {
    const own = names || this.names;
    const m = { kind: "script", url, id: null, disabling: false, game: this.gameOf(own), names: own };
    m.id = as || "(loading)";
    const def = this.asHost(() => factory(this.api(m)));
    if (!def || typeof def !== "object") throw new Error(`${url}: the factory returned no definition`);
    if (typeof def.id !== "string" || !/^[a-z0-9-]+$/.test(def.id)) throw new Error(`${url}: id: lowercase letters, digits and '-'`);
    m.id = def.id;
    m.version = def.version || "0.0.0";
    m.def = def;
    const known = ["id", "version", "load_after", "hooks", "data", "commands", "init", "game"];
    const extra = Object.keys(def).filter((k) => !known.includes(k));
    if (extra.length) throw new Error(`${m.id}: unknown fields ${extra.join(", ")} (a definition has ${known.join(", ")})`);

    // Each hooked slot's layer: befores, one core, afters, in order.
    const bySlot = new Map();
    for (const h of def.hooks || []) {
      if (!h || !h[HOOK]) throw new Error(`${m.id}: hooks holds hook(target, handlers) values`);
      let t = h.target;
      let bare = false;
      if (t.kind === "point") {
        const wrong = Object.keys(h.handlers).filter((k) => k !== t.point);
        if (wrong.length) throw new Error(`${m.id}: hook point ${t.name} is ${t.point} ${t.fn.name}: hook it with ${t.point}, not ${wrong.join(", ")}`);
        bare = true;
        t = t.fn;
      }
      if (t.slot === undefined) throw new Error(`${m.id}: ${t.name} (${t.container} ${hex(t.address)}) has no hook slot in this base: it's not a function, or the translator expands it into its callers`);
      if (h.handlers.replace && t.tier === "adapter") throw new Error(`${m.id}: ${t.name} is a runtime adapter: wrap it (before, after or around), don't replace it`);
      let L = bySlot.get(t.slot);
      if (!L) bySlot.set(t.slot, (L = { t, before: [], after: [], core: null }));
      for (const k of ["before", "after"]) if (h.handlers[k]) L[k].push({ fn: h.handlers[k], bare });
      for (const k of ["replace", "around"]) {
        if (!h.handlers[k]) continue;
        if (L.core) throw new Error(`${m.id}: two of its hooks take ${t.name}'s call (${L.core.kind} and ${k})`);
        L.core = { kind: k, fn: h.handlers[k], bare };
      }
    }
    const layers = [...bySlot.values()];

    // Its functions at synthetic addresses: commands, then fnptr values.
    const commands = Object.entries(def.commands || {});
    for (const [n, f] of commands) if (typeof f !== "function") throw new Error(`${m.id}: command ${n} isn't a function`);
    const functions = commands.map(([n, f]) => ({ name: n, fn: f, command: true }));
    const data = (def.data || []).map((d, i) => this.dataPatch(m, d, i, functions));
    const old = index !== undefined ? this.loader.mods[index] : null;
    const codeBase = old && old.kind === "script" && old.nFunctions >= functions.length ? old.codeBase : this.loader.synthetic(Math.max(functions.length, 1));

    const originals = layers.map(() => this.loader.x.psxmod_chain.grow(1, null));
    const shimTargets = layers.map((L, k) => ({ flavour: L.t.flavour, original: originals[k], before: L.before.length > 0, after: L.after.length > 0, core: L.core?.kind ?? null }));
    const module = new WebAssembly.Module(layer(shimTargets, functions.length));
    const j = {};
    let instance = null;
    layers.forEach((L, k) => Object.assign(j, this.handlers(m, L, k, () => instance)));
    j.init = () => this.initMod(m, data);
    j.loaded = (a, n) => this.loadedMod(m, data, a >>> 0, n >>> 0);
    functions.forEach((f, n) => (j[`fn.${n}`] = (a0, a1, a2, a3) => this.callFunction(m, f, [a0, a1, a2, a3])));
    instance = new WebAssembly.Instance(module, {
      b: { enter: this.x.psxw_js_enter, leave: this.x.psxw_js_leave, hi: this.hi, chain: this.loader.x.psxmod_chain },
      j,
    });
    functions.forEach((f, n) => {
      f.address = (codeBase + 4 * n) >>> 0;
      this.loader.x.psxmod_dispatch.set(this.loader.tableIndex(f.address), instance.exports[`fn.${n}`]);
    });
    Object.assign(m, {
      instance,
      slots: layers.map((L) => L.t.slot),
      originals,
      targets: layers.map((L) => ({ name: L.t.name, slot: L.t.slot, replaces: L.core?.kind === "replace", kinds: kindsOf(L) })),
      functions: functions.filter((f) => f.command).map((f) => [f.name, f.address]),
      commands: new Map(functions.filter((f) => f.command).map((f) => [f.name, f])),
      codeBase,
      nFunctions: Math.max(functions.length, 1),
      data,
    });
    const at = old ? index : this.loader.mods.findIndex((x) => x.id === m.id);
    if (old || at >= 0) this.loader.replace(old ? index : at, m);
    else this.loader.add(m);
    if (this.loader.booted) this.afterBoot(m);
    return m.id;
  }

  /** A mod loaded once the game runs: its data now, its init inside the game. */
  afterBoot(m) {
    this.applyData(m, m.data, false);
    if (typeof m.def.init === "function") {
      this.calls.enqueue(this.runAddress, [], { run: () => m.def.init() }).catch((e) => this.logLine(`mod ${m.id}: init: ${describe(e)}`));
    }
  }

  /** The base's `psxmod_init` reached the mod: boot loaded the executable. */
  initMod(m, data) {
    this.inside++;
    try {
      this.applyData(m, data, false);
      if (typeof m.def.init === "function") m.def.init();
    } catch (e) {
      this.fault(m, "init", e);
    } finally {
      this.inside--;
    }
  }

  /** The disc wrote `[a, a + n)`: overlay patches it covered are pending until their overlay is resident. */
  loadedMod(m, data, a, n) {
    const phys = a & 0x1fffff;
    for (const d of data) {
      if (d.main) continue;
      const at = d.address & 0x1fffff;
      if (at < phys + n && phys < at + d.size) d.pending = true;
    }
    for (const d of data) if (d.pending && this.isResident(d.container)) this.write(d);
  }

  isResident(container) {
    const r = this.resident.get(container);
    if (!r) return false;
    return this.mem.u32(r.at) === r.words[0] && this.mem.u32(r.at + 4) === r.words[1];
  }

  applyData(m, data) {
    for (const d of data) {
      if (d.main || this.isResident(d.container)) this.write(d);
    }
  }

  write(d) {
    d.pending = false;
    if (d.type === "bytes") this.mem.setBytes(d.address, d.value);
    else if (d.type === "fnptr") this.mem.setU32(d.address, d.fn.address);
    else this.mem[`set${d.type[0].toUpperCase()}${d.type.slice(1)}`](d.address, d.value);
  }

  /** A data patch: `{ target, offset, type, value }`. */
  dataPatch(m, d, i, functions) {
    const what = `data[${i}]`;
    if (!d || typeof d !== "object") throw new Error(`${m.id}: ${what} is { target, offset, type, value }`);
    let container;
    let address;
    if (typeof d.target === "number") address = d.target >>> 0;
    else if (d.target && d.target[TARGET]) ({ container, address } = d.target[TARGET]);
    else if (d.target && typeof d.target === "object") ({ container, address } = d.target);
    else throw new Error(`${m.id}: ${what}: target is an address (game.addr.name) or { container, address }`);
    address = (address + (d.offset ?? 0)) >>> 0;
    container ||= m.names.containerOf((typeof d.target === "number" ? d.target : address) >>> 0);
    if (!container) throw new Error(`${m.id}: ${what}: more than one overlay loads ${hex(address)}: give { container, address }`);
    const size = d.type === "bytes" ? (d.value || []).length : d.type === "fnptr" ? 4 : SCALARS[d.type]?.[0];
    if (!size) throw new Error(`${m.id}: ${what}: type ${d.type} isn't one of ${[...Object.keys(SCALARS), "bytes", "fnptr"].join(", ")}`);
    const out = { what, container, address, size, type: d.type, value: d.value, main: container === m.names.main, pending: false };
    if (d.type === "fnptr") {
      if (typeof d.value === "function" && d.value[TARGET]) {
        out.type = "u32";
        out.value = d.value[TARGET].address;
      } else if (typeof d.value === "function") {
        out.fn = functions.find((f) => f.fn === d.value) || { name: `${what}.fn`, fn: d.value, command: false };
        if (!functions.includes(out.fn)) functions.push(out.fn);
      } else throw new Error(`${m.id}: ${what}: a fnptr's value is a function`);
    } else if (d.type !== "bytes" && !Number.isInteger(d.value)) {
      throw new Error(`${m.id}: ${what}: ${d.type} wants an integer value`);
    }
    return out;
  }

  /** A command or fnptr function, called by the game (o32 words in, v0 out). */
  callFunction(m, f, words) {
    const c = this.calls.current();
    const mine = c && c.js && c.js.command === f;
    this.inside++;
    try {
      const args = mine ? c.js.args : words.map((w) => w >>> 0);
      const r = f.fn(...args);
      if (mine) c.js.result = r;
      return word(r, this.hi);
    } catch (e) {
      if (e && typeof e === "object" && this.fromGame.has(e)) throw e;
      if (mine) c.js.error = e;
      else this.fault(m, f.name, e);
      this.hi.value = 0;
      return 0;
    } finally {
      this.inside--;
    }
  }

  /** The JS functions a layer's shim imports for slot `k`. */
  handlers(m, L, k, instance) {
    const ty = slotType(L.t.flavour);
    const conv = (L.t.sig && L.t.types.signature(L.t.sig, ty, this.hi, () => this.x.__guest_sp.value)) || convert(ty, this.hi);
    const out = {};
    const name = L.t.name;
    if (L.before.length) {
      const hs = L.before;
      out[`${k}.before`] = (...w) => {
        this.inside++;
        try {
          const args = conv.args(w);
          for (const h of hs) h.bare ? h.fn() : h.fn(...args);
        } catch (e) {
          this.fault(m, `${name} before`, e);
        } finally {
          this.inside--;
        }
      };
    }
    if (L.after.length) {
      const hs = L.after;
      const nr = conv.resultWords;
      out[`${k}.after`] = (...w) => {
        this.inside++;
        try {
          const r = conv.result(w);
          const args = conv.args(w.slice(nr));
          for (const h of hs) h.bare ? h.fn() : h.fn(r, ...args);
        } catch (e) {
          this.fault(m, `${name} after`, e);
        } finally {
          this.inside--;
        }
      };
    }
    if (L.core) {
      const { kind, fn, bare } = L.core;
      const next = (w) => this.intoGame(() => instance().exports[`next.${k}`](...w));
      if (kind === "replace") {
        out[`${k}.core`] = (...w) => {
          this.inside++;
          try {
            return conv.out(bare ? fn() : fn(...conv.args(w)));
          } catch (e) {
            this.fault(m, `${name} replace`, e);
            return next(w);
          } finally {
            this.inside--;
          }
        };
      } else {
        out[`${k}.core`] = (...w) => {
          this.inside++;
          let last = null;
          const received = bare ? [] : conv.args(w);
          // The arguments it was given pass on as the words they came in.
          const same = (args) => args.length === received.length && args.every((a, i) => Object.is(a, received[i]));
          const original = (...args) => {
            const words = !args.length || same(args) ? w : conv.words(args);
            last = [next(words), this.hi.value];
            return conv.value(last[0]);
          };
          try {
            const r = bare ? fn(original) : fn(original, ...received);
            // Undefined passes the original's result on, whole.
            if (r === undefined && last) {
              this.hi.value = last[1];
              return last[0];
            }
            // An o32 result keeps the last original's v1 unless the handler
            // returns a bigint.
            return conv.out(r, last ? last[1] : 0);
          } catch (e) {
            this.fault(m, `${name} around`, e);
            if (last) {
              this.hi.value = last[1];
              return last[0];
            }
            return next(w);
          } finally {
            this.inside--;
          }
        };
      }
    }
    return out;
  }

  // --- the console -----------------------------------------------------------

  /** `psx`: hook, cmd, run, load, reload, unload, mods, struct, mem. */
  console() {
    if (this.psx) return this.psx;
    const host = this;
    this.psx = Object.freeze({
      mem: this.mem,
      struct: (fields) => this.struct(fields),
      /** Hook a target without writing a mod; active from the next vblank. */
      hook(target, handlers) {
        const id = `console-${++host.consoleN}`;
        const decl = host.hookDecl(target, handlers);
        const done = host.queue({ url: "(console)", factory: () => ({ id, hooks: [decl] }), as: id });
        return { id, ready: done, remove: () => host.pending.push({ op: "unload", id }) };
      },
      /** A command (script or compiled), inside the game at the next boundary. */
      cmd: (name, ...args) => this.command(name, args, true),
      /** `fn` inside the game at the next boundary; resolves with its result. */
      async run(fn) {
        if (typeof fn !== "function") throw new TypeError("psx.run(fn)");
        const { js } = await host.calls.enqueue(host.runAddress, [], { run: fn });
        if (js.error) throw js.error;
        return js.result;
      },
      load: (url) => this.load(url),
      reload: (id) => this.reload(id),
      unload: (id) =>
        new Promise((resolve) => {
          if (!this.loader.mods.some((m) => m.id === id)) throw new Error(`no mod ${id} is loaded`);
          this.pending.push({ op: "unload", id, resolve });
        }),
      mods: () => this.loader.describe(),
    });
    return this.psx;
  }

  /** Load a script mod (its mod.js URL, or a pack), after the others, at the next vblank. */
  async load(url) {
    if (typeof url === "object") return this.queue(await this.importPack(url));
    const abs = new URL(url, location.href).href;
    if (/\.psxpack\.json(\?|$)/.test(abs)) return this.queue(await this.importPack(await (await fetch(abs)).json()));
    return this.queue(await this.importMod(abs));
  }

  /** Re-import a loaded script mod and replace it in place at the next vblank. */
  async reload(id, url) {
    const i = this.loader.mods.findIndex((m) => m.id === id);
    if (i < 0) {
      if (url) return this.load(url);
      throw new Error(`no mod ${id} is loaded`);
    }
    const m = this.loader.mods[i];
    if (m.kind !== "script") throw new Error(`${id} is a compiled mod: reload it with psxw mod watch`);
    const base = new URL(url || m.url, location.href);
    if (!url) base.searchParams.set("reload", String(Date.now()));
    const item = await this.importMod(base.href);
    return this.queue({ ...item, index: this.loader.mods.findIndex((x) => x.id === id), replaced: true });
  }

  /**
   * A command by name, inside the game. Script commands get the JS
   * arguments and give back their JS result (`js`); compiled mods' global
   * functions take up to four words and give the raw u64 result.
   */
  command(name, args, js) {
    const found = [];
    for (const m of this.loader.mods) {
      if (m.kind === "script" && m.commands.has(name)) found.push({ m, f: m.commands.get(name) });
      else if (m.kind !== "script") for (const [n, a] of m.functions) if (n === name) found.push({ m, address: a });
    }
    if (!found.length) return Promise.reject(new Error(`no loaded mod has a command or global function ${name}`));
    if (found.length > 1) return Promise.reject(new Error(`${name} is in more than one loaded mod: ${found.map((f) => f.m.id).join(", ")}`));
    const { f, address } = found[0];
    if (f) {
      const words = js ? [] : args;
      return this.calls.enqueue(f.address, words.slice(0, 4), { command: f, args }).then((r) => {
        if (r.js.error) throw r.js.error;
        return js ? r.js.result : r.value;
      });
    }
    return this.calls.enqueue(address, args).then((r) => r.value);
  }
}

/** A JS value as an o32 result word (its high word into `hi`: a bigint's, else `v1`). */
function word(v, hi, v1 = 0) {
  if (typeof v === "bigint") {
    const b = BigInt.asUintN(64, v);
    hi.value = Number(b >> 32n) | 0;
    return Number(b & 0xffffffffn) | 0;
  }
  hi.value = v1;
  if (v === undefined || v === null) return 0;
  if (typeof v === "boolean") return v ? 1 : 0;
  return Number(v) | 0;
}

/** What a layer does on a slot, for `psx.mods()`. */
function kindsOf(L) {
  return [...L.before.map(() => "before"), ...(L.core ? [L.core.kind] : []), ...L.after.map(() => "after")];
}

/**
 * Converters between a slot's values as JS words (an i64 as two) and
 * what handlers take and give. Raw values (no declaration): a C slot's
 * wasm values, an i64 as a bigint; an o32 slot's argument words, and v0
 * for its result.
 */
function convert(ty, hi) {
  const i64 = (lo, h) => BigInt.asIntN(64, (BigInt(h >>> 0) << 32n) | BigInt(lo >>> 0));
  const split64 = (v) => {
    const b = BigInt.asUintN(64, BigInt(v ?? 0));
    return [Number(b & 0xffffffffn) | 0, Number(b >> 32n) | 0];
  };
  const r = ty.o32 ? null : ty.results[0];
  return {
    resultWords: ty.o32 ? 2 : r === "i64" ? 2 : r ? 1 : 0,
    args(w) {
      if (ty.o32) return [w[0], w[1], w[2], w[3]];
      const out = [];
      let i = 0;
      for (const p of ty.params) out.push(p === "i64" ? i64(w[i++], w[i++]) : w[i++]);
      return out;
    },
    /** `after`'s result, from its leading words. */
    result(w) {
      if (ty.o32) return w[0];
      if (r === "i64") return i64(w[0], w[1]);
      return r ? w[0] : undefined;
    },
    /** Handler arguments back to words, for `original`. */
    words(args) {
      if (ty.o32) return [0, 1, 2, 3].map((i) => Number(args[i] ?? 0) | 0);
      const out = [];
      ty.params.forEach((p, i) => (p === "i64" ? out.push(...split64(args[i])) : out.push(p === "f32" || p === "f64" ? Number(args[i] ?? 0) : Number(args[i] ?? 0) | 0)));
      return out;
    },
    /** A next link's result as `original` gives it back. */
    value(lo) {
      if (ty.o32) return lo;
      if (r === "i64") return i64(lo, hi.value);
      return r ? lo : undefined;
    },
    /** A handler's result as the shim takes it (`v1`: an o32 result's high word). */
    out(v, v1 = 0) {
      if (ty.o32) return word(v, hi, v1);
      if (r === "i64") {
        const [lo, h] = split64(v);
        hi.value = h;
        return lo;
      }
      if (r === "f32" || r === "f64") return Number(v ?? 0);
      return r ? Number(v ?? 0) | 0 : undefined;
    },
  };
}
