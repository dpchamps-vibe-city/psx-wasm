// One outstanding console call. Only messages cross the Worker boundary;
// game code runs on its own wasm stack, after the runtime releases its borrow.
export function scripting(worker) {
  let unavailable = "game is not ready; wait for its first vblank";
  let pending = null;
  let nextId = 0;
  const fail = (reason) => {
    unavailable = reason;
    if (pending) {
      pending.reject(new Error(reason));
      pending = null;
    }
  };
  worker.addEventListener("message", ({ data: m }) => {
    if (m.type === "scripting-ready") unavailable = m.error || null;
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
    async execute(address, ...args) {
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
