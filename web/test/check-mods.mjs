// `psxw mod check` for script mods (plans/M12): run each mod's factory
// against a stub of the mod API (web/script.js) and print its definition
// as JSON, names unresolved: psxw resolves them against the snapshot and
// plans them with compiled mods. Runs in Node in the browsers image.
//
// Usage: node check-mods.mjs DIR...  (each DIR holds a mod.js)
// Output: [{ dir, def } | { dir, error }] on stdout.

import { cpSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const MOD = Symbol.for("psxw.mod");
const TARGET = Symbol.for("psxw.target");
const HOOK = Symbol.for("psxw.hook");
const KINDS = ["before", "after", "replace", "around"];
const hex = (a) => "0x" + (a >>> 0).toString(16).toUpperCase().padStart(8, "0");

globalThis.mod = (factory) => {
  if (typeof factory !== "function") throw new TypeError("mod(factory): give it a function that returns the mod's definition");
  return { [MOD]: factory };
};

// Addresses stand for names: `game.addr.X + n` decodes to (X, n).
const FAKE = 0x40000000;
const SPAN = 0x100000;

function stub() {
  const names = [];
  const addrOf = (name) => {
    let k = names.indexOf(name);
    if (k < 0) k = names.push(name) - 1;
    return FAKE + k * SPAN;
  };
  const decode = (a) => {
    const off = a - FAKE;
    if (!Number.isInteger(a) || off < 0 || off >= names.length * SPAN) return null;
    return { symbol: names[Math.floor(off / SPAN)], offset: off % SPAN };
  };
  const target = (t) => {
    const f = () => {
      throw new Error(`${t.symbol || t.point}: game functions run only inside the game (in a hook, a command, init, or psx.run)`);
    };
    f[TARGET] = t;
    return f;
  };
  const named = (get) => new Proxy({}, { get: (_, k) => (typeof k === "symbol" ? undefined : get(k)) });
  const view = () => named(() => 0);
  const record = { at: () => view() };
  const game = named((k) => {
    switch (k) {
      case "addr":
        return named(addrOf);
      case "points":
        return named((p) => ({ [TARGET]: { point: p } }));
      case "at":
        return (container, where) => target(typeof where === "string" ? { container, symbol: where } : { container, address: hex(where) });
      case "types":
        return named(() => record);
      case "snapshot":
        return {};
      default:
        return target({ symbol: k });
    }
  });
  const mem = named(() => () => 0);
  const hook = (t, handlers) => {
    const tt = t && t[TARGET];
    if (!tt) throw new TypeError(`hook(target, handlers): ${String(t)} isn't a hook target (game.Name, game.points.name, game.at(container, address))`);
    if (!handlers || typeof handlers !== "object") throw new TypeError("hook(target, handlers): handlers is an object");
    const kinds = Object.keys(handlers);
    const bad = kinds.filter((k) => !KINDS.includes(k) || typeof handlers[k] !== "function");
    if (bad.length) throw new TypeError(`hook(${tt.symbol || tt.point}): ${bad.join(", ")}: handlers are functions named ${KINDS.join(", ")}`);
    if (!kinds.length) throw new TypeError(`hook(${tt.symbol || tt.point}): no handlers`);
    if (handlers.replace && handlers.around) throw new TypeError(`hook(${tt.symbol || tt.point}): replace and around both take the call; give one`);
    return { [HOOK]: true, target: tt, kinds };
  };
  const psx = { mem, struct: () => record, hook: () => ({ remove() {} }), cmd: async () => {}, run: async () => {}, load: async () => {}, reload: async () => {}, unload: async () => {}, mods: () => [] };
  return { api: { game, mem, hook, log() {}, psx }, decode };
}

async function check(dir) {
  // A copy beside a package.json, so Node reads mod.js as a module.
  const tmp = mkdtempSync(join(tmpdir(), "psxw-mod-"));
  cpSync(dir, tmp, { recursive: true });
  writeFileSync(join(tmp, "package.json"), '{"type":"module"}');
  const module = await import(pathToFileURL(join(tmp, "mod.js")).href);
  const d = module.default;
  if (!d || !d[MOD]) throw new Error("its default export isn't mod(factory)");
  const { api, decode } = stub();
  const def = d[MOD](api);
  if (!def || typeof def !== "object") throw new Error("the factory returned no definition");
  const known = ["id", "version", "load_after", "hooks", "data", "commands", "init", "game"];
  const extra = Object.keys(def).filter((k) => !known.includes(k));
  if (extra.length) throw new Error(`unknown fields ${extra.join(", ")} (a definition has ${known.join(", ")})`);
  const hooks = (def.hooks || []).map((h, i) => {
    if (!h || !h[HOOK]) throw new Error(`hooks[${i}] isn't hook(target, handlers)`);
    return { target: h.target, kinds: h.kinds };
  });
  const data = (def.data || []).map((x, i) => {
    let target;
    let offset = x.offset ?? 0;
    if (typeof x.target === "number") {
      const n = decode(x.target);
      if (n) {
        target = { symbol: n.symbol };
        offset += n.offset;
      } else target = { address: hex(x.target) };
    } else if (x.target && x.target[TARGET]) target = x.target[TARGET];
    else if (x.target && typeof x.target === "object") target = { container: x.target.container, address: hex(x.target.address) };
    else throw new Error(`data[${i}]: target is an address (game.addr.name) or { container, address }`);
    if (x.type === "fnptr" && typeof x.value !== "function") throw new Error(`data[${i}]: a fnptr's value is a function`);
    const value = x.type === "fnptr" ? { file: "mod.js", function: `data${i}` } : x.value;
    return { target, offset, type: x.type, value };
  });
  const commands = Object.entries(def.commands || {}).map(([n, f]) => {
    if (typeof f !== "function") throw new Error(`command ${n} isn't a function`);
    return n;
  });
  return { id: def.id, version: def.version, load_after: def.load_after || [], hooks, data, commands, init: typeof def.init === "function" };
}

const out = [];
for (const dir of process.argv.slice(2)) {
  try {
    out.push({ dir, def: await check(dir) });
  } catch (e) {
    out.push({ dir, error: String((e && e.stack) || e) });
  }
}
process.stdout.write(JSON.stringify(out));
