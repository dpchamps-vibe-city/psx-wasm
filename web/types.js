// Types from a snapshot (plans/M12 phase 2, D55): typed globals, views of
// records and arrays in guest memory, record types, and handlers' values
// converted by a function's declared signature. Type expressions are data:
// a width ("u8" … "s64", "f32", "f64", "void"), { ptr }, { array, count },
// { record }, { named } (an enum or typedef) or { fn }.

const ADDR = Symbol.for("psxw.address");
const WIDTHS = {
  u8: [1, "getUint8", "setUint8"],
  s8: [1, "getInt8", "setInt8"],
  u16: [2, "getUint16", "setUint16"],
  s16: [2, "getInt16", "setInt16"],
  u32: [4, "getUint32", "setUint32"],
  s32: [4, "getInt32", "setInt32"],
  u64: [8, "getBigUint64", "setBigUint64"],
  s64: [8, "getBigInt64", "setBigInt64"],
  f32: [4, "getFloat32", "setFloat32"],
  f64: [8, "getFloat64", "setFloat64"],
};

/** A width as a JS value from a raw 32-bit word (sign or zero extended). */
const fromWord = {
  u8: (v) => v & 0xff,
  s8: (v) => (v << 24) >> 24,
  u16: (v) => v & 0xffff,
  s16: (v) => (v << 16) >> 16,
  u32: (v) => v >>> 0,
  s32: (v) => v | 0,
};

export class Types {
  /** `mem()`: a DataView of the module's memory; `window`: the guest window's wasm address. */
  constructor(snapshot, mem, window) {
    this.defs = snapshot.types || {};
    this.mem = mem;
    this.window = window;
    this.protos = new Map();
    this.recordTypes = null;
  }

  at(a) {
    return this.window + (a & 0xffffff);
  }

  /** A type with typedefs and enums seen through: a width, or the expression. */
  resolve(t) {
    for (let i = 0; i < 16 && t && typeof t === "object" && "named" in t; i++) {
      const d = this.defs[t.named];
      if (!d) return "u32";
      if (d.kind === "enum") return d.size === 1 ? "s8" : d.size === 2 ? "s16" : "s32";
      if (d.kind !== "typedef") return "u32";
      t = d.type;
    }
    return t;
  }

  /** A scalar's width, or null for an aggregate. Pointers and functions are u32. */
  width(t) {
    t = this.resolve(t);
    if (typeof t === "string") return WIDTHS[t] ? t : null;
    if ("ptr" in t || "fn" in t) return "u32";
    return null;
  }

  size(t) {
    t = this.resolve(t);
    if (typeof t === "string") return WIDTHS[t]?.[0] ?? 0;
    if ("ptr" in t || "fn" in t) return 4;
    if ("array" in t) return this.size(t.array) * t.count;
    if ("record" in t) return this.defs[t.record]?.size ?? 0;
    return 0;
  }

  read(width, a) {
    const [, get] = WIDTHS[width];
    return this.mem()[get](this.at(a), true);
  }

  write(width, a, v) {
    const [, , set] = WIDTHS[width];
    this.mem()[set](this.at(a), width.endsWith("64") ? BigInt.asIntN(64, BigInt(v)) : Number(v), true);
  }

  /** What a field or element of type `t` at `a` reads as: a value, or a view. */
  get(t, a) {
    const w = this.width(t);
    if (w) return this.read(w, a);
    return this.view(t, a);
  }

  set(t, a, v, what) {
    const w = this.width(t);
    if (!w) throw new TypeError(`${what} is an aggregate: write its fields or elements`);
    this.write(w, a, v);
  }

  /** A view of a record or an array at guest address `a`. */
  view(t, a) {
    t = this.resolve(t);
    if (typeof t === "object" && "record" in t) return this.recordView(t.record, a);
    if (typeof t === "object" && "array" in t) return this.arrayView(t, a);
    throw new TypeError(`no view of ${JSON.stringify(t)}`);
  }

  recordView(id, a) {
    let proto = this.protos.get(id);
    if (!proto) {
      const def = this.defs[id];
      if (!def || !def.fields) throw new TypeError(`no layout for ${id} in this snapshot`);
      proto = {};
      const types = this;
      for (const f of def.fields) {
        const what = `${id}.${f.name}`;
        Object.defineProperty(proto, f.name, {
          enumerable: true,
          get() {
            return types.get(f.type, this[ADDR] + f.offset);
          },
          set(v) {
            types.set(f.type, this[ADDR] + f.offset, v, what);
          },
        });
      }
      Object.defineProperty(proto, "toJSON", {
        value() {
          return Object.fromEntries(def.fields.map((f) => [f.name, this[f.name]]));
        },
      });
      this.protos.set(id, proto);
    }
    const v = Object.create(proto);
    Object.defineProperty(v, ADDR, { value: a >>> 0 });
    return v;
  }

  arrayView(t, a) {
    const types = this;
    const el = t.array;
    const step = this.size(el);
    const base = a >>> 0;
    const target = {
      [ADDR]: base,
      length: t.count,
      at(i) {
        return types.get(el, base + i * step);
      },
      *[Symbol.iterator]() {
        for (let i = 0; i < t.count; i++) yield this.at(i);
      },
    };
    const index = (k) => (typeof k === "string" && /^\d+$/.test(k) ? Number(k) : -1);
    return new Proxy(target, {
      get(o, k) {
        const i = index(k);
        if (i < 0) return o[k];
        if (t.count && i >= t.count) return undefined;
        return types.get(el, base + i * step);
      },
      set(o, k, v) {
        const i = index(k);
        if (i < 0 || (t.count && i >= t.count)) return false;
        types.set(el, base + i * step, v, `[${i}]`);
        return true;
      },
    });
  }

  /** `game.types`: a record type by name (`Player`, `struct Tag` as `Tag`). */
  records() {
    if (this.recordTypes) return this.recordTypes;
    const out = {};
    const add = (name, id) => {
      if (name in out) return;
      const def = this.defs[id];
      out[name] = Object.freeze({ name: id, size: def.size, fields: def.fields, at: (address) => this.recordView(id, address) });
    };
    for (const [id, d] of Object.entries(this.defs)) {
      if (d.kind === "struct" || d.kind === "union") add(id.replace(/^(struct|union) /, ""), id);
    }
    for (const [id, d] of Object.entries(this.defs)) {
      if (d.kind === "typedef" && d.type && typeof d.type === "object" && "record" in d.type && this.defs[d.type.record]?.fields) add(id, d.type.record);
    }
    this.recordTypes = out;
    return out;
  }

  /**
   * Converters for a function's slot by its declared signature (null when
   * the declaration doesn't fit the slot: raw values then). `ty`: the slot
   * type (shims.js `slotType`); `sp()`: the guest stack pointer.
   */
  signature(sig, ty, hi, sp) {
    if (!sig) return null;
    const result = this.resolve(sig.result);
    const rw = result === "void" ? null : this.width(result);
    if (result !== "void" && !rw) return null;
    const params = sig.params.map((p) => this.width(p.type));
    if (params.some((p) => !p) || sig.variadic) return null;
    const is64 = (w) => w === "u64" || w === "s64" || w === "f64";
    const value = (w, lo, h) => {
      if (w === "u64" || w === "s64") return BigInt[w === "u64" ? "asUintN" : "asIntN"](64, (BigInt(h >>> 0) << 32n) | BigInt(lo >>> 0));
      if (w === "f32") return new Float32Array(new Uint32Array([lo >>> 0]).buffer)[0];
      if (w === "f64") return new Float64Array(new Uint32Array([lo >>> 0, h >>> 0]).buffer)[0];
      return fromWord[w](lo);
    };
    const split = (w, v) => {
      if (w === "u64" || w === "s64") {
        const b = BigInt.asUintN(64, BigInt(v ?? 0));
        return [Number(b & 0xffffffffn) | 0, Number(b >> 32n) | 0];
      }
      if (w === "f32") return [new Uint32Array(new Float32Array([Number(v ?? 0)]).buffer)[0] | 0];
      if (w === "f64") {
        const u = new Uint32Array(new Float64Array([Number(v ?? 0)]).buffer);
        return [u[0] | 0, u[1] | 0];
      }
      return [Number(v ?? 0) | 0];
    };
    if (!ty.o32) {
      // A C slot: one wasm value per parameter, in the declared order.
      const wasm = (w) => (w === "u64" || w === "s64" ? "i64" : w === "f32" || w === "f64" ? w : "i32");
      if (ty.params.length !== params.length || (ty.results.length > 0) !== (rw !== null)) return null;
      if (!params.every((w, i) => wasm(w) === ty.params[i]) || (rw && wasm(rw) !== ty.results[0])) return null;
      const n = (w) => (w === "u64" || w === "s64" ? 2 : 1);
      const rn = rw ? n(rw) : 0;
      const wasmValue = (w, words, i) => {
        if (w === "u64" || w === "s64") return value(w, words[i], words[i + 1]);
        if (w === "f32" || w === "f64") return words[i];
        return fromWord[w](words[i]);
      };
      return {
        resultWords: rn,
        args(words) {
          const out = [];
          let i = 0;
          for (const w of params) {
            out.push(wasmValue(w, words, i));
            i += n(w);
          }
          return out;
        },
        result: (words) => (rw ? wasmValue(rw, words, 0) : undefined),
        words(args) {
          return params.flatMap((w, i) => (w === "f32" || w === "f64" ? [Number(args[i] ?? 0)] : split(w, args[i])));
        },
        list(args) {
          return this.words(args);
        },
        value: (lo) => (rw ? wasmValue(rw, [lo, hi.value], 0) : undefined),
        out(v) {
          if (!rw) return undefined;
          if (rw === "f32" || rw === "f64") return Number(v ?? 0);
          const [lo, h] = split(rw, v);
          if (rw === "u64" || rw === "s64") hi.value = h;
          return lo;
        },
      };
    }
    // An o32 slot: the declared parameters' words (64-bit values in an
    // even-aligned pair), past the fourth on the caller's stack at sp + 4k.
    const at = [];
    let k = 0;
    for (const w of params) {
      if (is64(w) && k % 2) k++;
      at.push(k);
      k += is64(w) ? 2 : 1;
    }
    const types = this;
    const word = (regs, i) => (i < 4 ? regs[i] : types.read("u32", sp() + 4 * i));
    return {
      resultWords: 2,
      args(regs) {
        return params.map((w, j) => value(w, word(regs, at[j]), word(regs, at[j] + 1)));
      },
      result: (w) => (rw ? value(rw, w[0], w[1]) : undefined),
      /** Every word, for a call that sets up its own frame. */
      list(args) {
        const out = [0, 0, 0, 0];
        params.forEach((w, j) => split(w, args[j]).forEach((x, d) => (out[at[j] + d] = x)));
        return out.map((x) => x | 0);
      },
      /** a0..a3, the rest written where the next link reads them (the caller's frame). */
      words(args) {
        const out = this.list(args);
        for (let i = 4; i < out.length; i++) types.write("u32", sp() + 4 * i, out[i]);
        return out.slice(0, 4);
      },
      value: (lo) => (rw ? value(rw, lo, hi.value) : undefined),
      out(v, v1 = 0) {
        if (!rw) {
          hi.value = v1;
          return 0;
        }
        const [lo, h] = split(rw, v);
        hi.value = is64(rw) ? h : v1;
        return lo;
      },
    };
  }
}
