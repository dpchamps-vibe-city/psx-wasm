// Console calls into the game Worker. Only messages cross the Worker
// boundary; game code runs on its own wasm stack, after the runtime
// releases its borrow. `execute` takes one call at a time; `cmd` and
// `load` (script mods, plans/M12) are queued in the Worker.
export function scripting(worker) {
  let unavailable = "game is not ready; wait for its first vblank";
  let pending = null;
  let nextId = 0;
  const waiting = new Map();
  const ask = (type, body) => {
    if (unavailable) return Promise.reject(new Error(unavailable));
    return new Promise((resolve, reject) => {
      const id = ++nextId;
      waiting.set(id, { resolve, reject });
      worker.postMessage({ type, id, ...body });
    });
  };
  // The loaded mods' global functions, by name (from the Worker).
  let functions = {};
  const fail = (reason) => {
    unavailable = reason;
    if (pending) {
      pending.reject(new Error(reason));
      pending = null;
    }
    for (const w of waiting.values()) w.reject(new Error(reason));
    waiting.clear();
  };
  worker.addEventListener("message", ({ data: m }) => {
    if (m.type === "cmd-result" || m.type === "load-result") {
      const w = waiting.get(m.id);
      waiting.delete(m.id);
      if (w) m.error ? w.reject(new Error(m.error)) : w.resolve(m.value);
    } else if (m.type === "scripting-ready") unavailable = m.error || null;
    else if (m.type === "mods") functions = m.functions || {};
    else if (m.type === "execute-result" && pending?.id === m.id) {
      const p = pending;
      pending = null;
      if (m.error) p.reject(new Error(m.error));
      else p.resolve(BigInt(m.value));
    } else if (m.type === "stopped") fail(`game stopped: ${m.reason}`);
  });
  worker.addEventListener("error", () => fail("game Worker failed"));
  worker.addEventListener("messageerror", () => fail("game Worker message could not be read"));

  return Object.freeze({
    /** A script mod's command or a compiled mod's global function, by name; its result. */
    cmd: (name, ...args) => ask("cmd", { name, args }),
    /** Load a script mod (its mod.js URL, or a pack's JSON) after the others, at the next vblank; its id. */
    load: (url) => ask("load", typeof url === "object" ? { pack: url } : { url: new URL(url, location.href).href }),
    /** Re-import loaded script mod `id` (from `url`, or where it came from). */
    reload: (id, url) => ask("load", { reload: id, url: url && new URL(url, location.href).href }),
    async execute(address, ...args) {
      if (typeof address === "string") {
        const found = functions[address];
        if (!found) throw new Error(`no loaded mod defines a global function or command ${address}`);
        if (found.length > 1) throw new Error(`more than one loaded mod defines ${address}`);
        address = found[0];
      }
      const word = (v) => Number.isInteger(v) && v >= 0 && v <= 0xffffffff;
      if (!word(address) || !address || address % 4 || (address & 0xffffff) >= 0x400000) {
        throw new Error("address must be an aligned guest function address in RAM or the synthetic code range");
      }
      if (args.length > 4 || !args.every(word)) throw new Error("execute takes up to four unsigned 32-bit argument words");
      if (unavailable) throw new Error(unavailable);
      if (pending) throw new Error("an execute call is already pending");
      return new Promise((resolve, reject) => {
        const id = ++nextId;
        pending = { id, resolve, reject };
        try {
          worker.postMessage({ type: "execute", id, address, args });
        } catch (e) {
          pending = null;
          reject(e);
        }
      });
    },
  });
}
