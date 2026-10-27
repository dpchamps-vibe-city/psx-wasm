// Wasm for script mods (plans/M12, D54), written here byte by byte, so a
// script mod needs no build step:
// - a mod's layer (`layer`): `hook.<k>` for each slot it hooks, in the
//   slot's own type, so it chains like a compiled mod's layer (D51);
//   `next.<k>` for an `around`'s `original`; `psxmod_init` and
//   `psxmod_loaded`; `fn.<n>`, o32 entries for its commands and `fnptr`
//   values at synthetic addresses;
// - the host's helpers (`calls`): JS calling into the game, through a
//   slot's entry in its own type or through the dispatch table (o32), and
//   `run`, the entry `psx.run` callbacks run through.
//
// Every call into JS is bracketed with the runtime's `psxw_js_enter` and
// `psxw_js_leave`, `leave` also when an exception unwinds through it, so
// the runtime holds the host's vblank wait while JS is on the stack.
//
// i64 values cross as two i32 words, so no BigInt crosses per call: a
// parameter as (lo, hi), a result as lo with hi in the shared global
// `hi`.

const I32 = 0x7f;
const I64 = 0x7e;
const F32 = 0x7d;
const F64 = 0x7c;
const FUNCREF = 0x70;
const EXNREF = 0x69;
const VAL = { i32: I32, i64: I64, f32: F32, f64: F64 };

const uleb = (n) => {
  const out = [];
  n >>>= 0;
  do {
    let b = n & 0x7f;
    n >>>= 7;
    if (n) b |= 0x80;
    out.push(b);
  } while (n);
  return out;
};
const sleb = (n) => {
  const out = [];
  n |= 0;
  for (;;) {
    const b = n & 0x7f;
    n >>= 7;
    if ((n === 0 && !(b & 0x40)) || (n === -1 && b & 0x40)) {
      out.push(b);
      return out;
    }
    out.push(b | 0x80);
  }
};
const name = (s) => {
  const b = new TextEncoder().encode(s);
  return [...uleb(b.length), ...b];
};
const vec = (items) => [...uleb(items.length), ...items.flat()];
const section = (id, body) => (body.length ? [id, ...uleb(body.length), ...body] : []);

// Instructions.
const op = {
  localGet: (i) => [0x20, ...uleb(i)],
  localSet: (i) => [0x21, ...uleb(i)],
  localTee: (i) => [0x22, ...uleb(i)],
  globalGet: (i) => [0x23, ...uleb(i)],
  globalSet: (i) => [0x24, ...uleb(i)],
  call: (f) => [0x10, ...uleb(f)],
  callIndirect: (t, table) => [0x11, ...uleb(t), ...uleb(table)],
  i32: (v) => [0x41, ...sleb(v)],
  i64: (v) => [0x42, ...sleb(v)],
  i32And: [0x71],
  i32ShrU: [0x76],
  i64Or: [0x84],
  i64Shl: [0x86],
  i64ShrU: [0x88],
  wrap: [0xa7],
  extendU: [0xad],
  block: (bt) => [0x02, bt],
  end: [0x0b],
  br: (l) => [0x0c, ...uleb(l)],
  tryCatchAllRef: (label) => [0x1f, 0x40, ...uleb(1), 0x03, ...uleb(label)],
  throwRef: [0x0a],
};

/** A module under construction: imports first, then functions. */
class Builder {
  constructor() {
    this.types = [];
    this.typeIndex = new Map();
    this.imports = [];
    this.funcs = [];
    this.exports = [];
    this.nFuncImports = 0;
    this.nTables = 0;
    this.nGlobals = 0;
  }

  type(params, results) {
    const key = `${params.join(",")}>${results.join(",")}`;
    if (!this.typeIndex.has(key)) {
      this.typeIndex.set(key, this.types.length);
      this.types.push([0x60, ...vec(params.map((t) => [t])), ...vec(results.map((t) => [t]))]);
    }
    return this.typeIndex.get(key);
  }

  importFunc(mod, field, params, results) {
    this.imports.push([...name(mod), ...name(field), 0x00, ...uleb(this.type(params, results))]);
    return this.nFuncImports++;
  }

  importTable(mod, field) {
    this.imports.push([...name(mod), ...name(field), 0x01, FUNCREF, 0x00, 0x00]);
    return this.nTables++;
  }

  importGlobal(mod, field, t, mutable) {
    this.imports.push([...name(mod), ...name(field), 0x03, t, mutable ? 1 : 0]);
    return this.nGlobals++;
  }

  /** A function: its locals beyond the parameters, as types, and its code. */
  func(params, results, locals, code) {
    const groups = locals.map((t) => [1, t]);
    const body = [...vec(groups), ...code.flat(), ...op.end];
    this.funcs.push({ type: this.type(params, results), body: [...uleb(body.length), ...body] });
    return this.nFuncImports + this.funcs.length - 1;
  }

  exportFunc(field, index) {
    this.exports.push([...name(field), 0x00, ...uleb(index)]);
  }

  bytes() {
    return new Uint8Array([
      0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
      ...section(1, vec(this.types)),
      ...section(2, vec(this.imports)),
      ...section(3, vec(this.funcs.map((f) => uleb(f.type)))),
      ...section(7, vec(this.exports)),
      ...section(10, vec(this.funcs.map((f) => f.body))),
    ]);
  }
}

/**
 * A slot's type, from its flavour in `psxmod.slots`: `c <params>-><results>`
 * (wasm value types) or `o32`, the dispatch table's `(i32 x4) -> i64`.
 */
export function slotType(flavour) {
  if (flavour === "o32") return { o32: true, params: ["i32", "i32", "i32", "i32"], results: ["i64"] };
  const m = /^c (.*)->(.*)$/.exec(flavour);
  if (!m) throw new Error(`unknown slot flavour ${flavour}`);
  const list = (s) => (s ? s.split(",") : []);
  return { o32: false, params: list(m[1]), results: list(m[2]) };
}

/** Value types as JS sees them: an i64 as two i32 words. */
const split = (types) => types.flatMap((t) => (t === "i64" ? [I32, I32] : [VAL[t]]));
/** A result as JS returns it: an i64's low word (the high one in `hi`). */
const jsResult = (results) => results.map((t) => (t === "i64" ? I32 : VAL[t]));

/** Push local `i` (of type `t`) as JS takes it. */
const pushSplit = (i, t) =>
  t === "i64" ? [op.localGet(i), op.wrap, op.localGet(i), op.i64(32), op.i64ShrU, op.wrap] : [op.localGet(i)];

/** Push an i64 from JS words: locals `lo`, `hi`. */
const joinLocals = (lo, hi) => [op.localGet(lo), op.extendU, op.localGet(hi), op.extendU, op.i64(32), op.i64Shl, op.i64Or];

/** An i32 from JS on the stack, and global `hi`, as an i64. */
const joinHi = (hi) => [op.extendU, op.globalGet(hi), op.extendU, op.i64(32), op.i64Shl, op.i64Or];

/** An i64 on the stack to JS: its high word into global `hi`, low word left. */
const toJs = (tmp, hi) => [op.localTee(tmp), op.i64(32), op.i64ShrU, op.wrap, op.globalSet(hi), op.localGet(tmp), op.wrap];

/**
 * `code` bracketed by enter/leave; leave also runs when an exception
 * unwinds through it (rethrown as it was). `exn` is an exnref local.
 */
const bracket = (fns, exn, code) => [
  op.call(fns.enter),
  op.block(0x40),
  op.block(EXNREF),
  op.tryCatchAllRef(0),
  ...code,
  op.end,
  op.call(fns.leave),
  op.br(1),
  op.end,
  op.localSet(exn),
  op.call(fns.leave),
  op.localGet(exn),
  op.throwRef,
  op.end,
];

/** Imports every shim takes: the runtime's bracket and the `hi` word. */
function common(b) {
  const enter = b.importFunc("b", "enter", [], []);
  const leave = b.importFunc("b", "leave", [], []);
  return { enter, leave };
}

/**
 * A script mod's layer module. `targets[k]`: `{ flavour, original, before,
 * after, core }`: the slot's flavour, the chain index of its next link,
 * and which handlers it has (`core`: "replace", "around" or null).
 * `functions`: how many o32 entries (`fn.<n>`) to make.
 *
 * Imports: `b.enter`, `b.leave`, `b.hi` (mutable i32), `b.chain` (the
 * base's chain table), and from `j`: `<k>.before`, `<k>.after`,
 * `<k>.core`, `init`, `loaded`, `fn.<n>`.
 */
export function layer(targets, functions) {
  const b = new Builder();
  const fns = common(b);
  const imp = targets.map((t, k) => {
    const ty = slotType(t.flavour);
    const r = ty.results;
    return {
      ty,
      before: t.before ? b.importFunc("j", `${k}.before`, split(ty.params), []) : null,
      after: t.after ? b.importFunc("j", `${k}.after`, [...split(r), ...split(ty.params)], []) : null,
      core: t.core ? b.importFunc("j", `${k}.core`, split(ty.params), jsResult(r)) : null,
    };
  });
  const init = b.importFunc("j", "init", [], []);
  const loaded = b.importFunc("j", "loaded", [I32, I32], []);
  const fnImports = [];
  for (let n = 0; n < functions; n++) fnImports.push(b.importFunc("j", `fn.${n}`, [I32, I32, I32, I32], [I32]));
  const hi = b.importGlobal("b", "hi", I32, true);
  const chain = b.importTable("b", "chain");

  targets.forEach((t, k) => {
    const { ty, before, after, core } = imp[k];
    const P = ty.params.map((v) => VAL[v]);
    const R = ty.results.map((v) => VAL[v]);
    const slotTy = b.type(P, R);
    const n = P.length;
    const hasR = R.length > 0;
    const res = n;
    const exn = n + (hasR ? 1 : 0);
    const tmp = exn + 1;
    const locals = [...R, EXNREF, I64];
    const params = () => ty.params.flatMap((v, i) => pushSplit(i, v));
    const code = [];
    if (before !== null) code.push(...bracket(fns, exn, [...params(), op.call(before)]));
    if (core !== null) {
      const back = ty.results[0] === "i64" ? joinHi(hi) : [];
      code.push(...bracket(fns, exn, [...params(), op.call(core), ...back, ...(hasR ? [op.localSet(res)] : [])]));
    } else {
      code.push(...ty.params.map((_, i) => op.localGet(i)), op.i32(t.original), op.callIndirect(slotTy, chain));
      if (hasR) code.push(op.localSet(res));
    }
    if (after !== null) {
      const r = hasR ? pushSplit(res, ty.results[0]) : [];
      code.push(...bracket(fns, exn, [...r, ...params(), op.call(after)]));
    }
    if (hasR) code.push(op.localGet(res));
    b.exportFunc(`hook.${k}`, b.func(P, R, locals, code));

    // `original` for an `around`: the next link, from JS.
    if (core !== null) {
      const S = split(ty.params);
      const args = [];
      let w = 0;
      for (const v of ty.params) {
        if (v === "i64") {
          args.push(...joinLocals(w, w + 1));
          w += 2;
        } else args.push(op.localGet(w++));
      }
      const call = [...args, op.i32(t.original), op.callIndirect(slotTy, chain)];
      const out = ty.results[0] === "i64" ? toJs(S.length, hi) : [];
      b.exportFunc(`next.${k}`, b.func(S, jsResult(ty.results), [I64], [...call, ...out]));
    }
  });

  b.exportFunc("psxmod_init", b.func([], [], [EXNREF], bracket(fns, 0, [op.call(init)])));
  b.exportFunc("psxmod_loaded", b.func([I32, I32], [], [EXNREF], bracket(fns, 2, [op.localGet(0), op.localGet(1), op.call(loaded)])));
  fnImports.forEach((f, n) => b.exportFunc(`fn.${n}`, b.func(...o32Entry(fns, hi, f))));
  return b.bytes();
}

/** An o32 function `(a0..a3) -> i64` calling JS `f` (its result's high word in `hi`). */
function o32Entry(fns, hi, f) {
  const res = 4;
  const exn = 5;
  const code = [
    ...bracket(fns, exn, [op.localGet(0), op.localGet(1), op.localGet(2), op.localGet(3), op.call(f), ...joinHi(hi), op.localSet(res)]),
    op.localGet(res),
  ];
  return [[I32, I32, I32, I32], [I64], [I64, EXNREF], code];
}

/**
 * The host's helpers for a base: `c:<flavour>` for each C-typed slot
 * flavour, `(entry, ...params as JS words) -> result as JS takes it`,
 * calling `psxmod_entries[entry]`; `o32(address, a0..a3) -> v0` (v1 in
 * `hi`), calling the dispatch table; and `run`, an o32 entry that calls JS
 * `j.run`.
 *
 * Imports: `b.enter`, `b.leave`, `b.hi`, `b.entries`, `b.dispatch`,
 * `j.run`.
 */
export function calls(flavours) {
  const b = new Builder();
  const fns = common(b);
  const run = b.importFunc("j", "run", [I32, I32, I32, I32], [I32]);
  const hi = b.importGlobal("b", "hi", I32, true);
  const entries = b.importTable("b", "entries");
  const dispatch = b.importTable("b", "dispatch");
  for (const flavour of flavours) {
    const ty = slotType(flavour);
    if (ty.o32) continue;
    const P = ty.params.map((v) => VAL[v]);
    const R = ty.results.map((v) => VAL[v]);
    const S = split(ty.params);
    const args = [];
    let w = 1;
    for (const v of ty.params) {
      if (v === "i64") {
        args.push(...joinLocals(w, w + 1));
        w += 2;
      } else args.push(op.localGet(w++));
    }
    const out = ty.results[0] === "i64" ? toJs(S.length + 1, hi) : [];
    const code = [...args, op.localGet(0), op.callIndirect(b.type(P, R), entries), ...out];
    b.exportFunc(`c:${flavour}`, b.func([I32, ...S], jsResult(ty.results), [I64], code));
  }
  const o32 = b.type([I32, I32, I32, I32], [I64]);
  const code = [
    op.localGet(1), op.localGet(2), op.localGet(3), op.localGet(4),
    op.localGet(0), op.i32(0x003ffffc), op.i32And, op.i32(2), op.i32ShrU,
    op.callIndirect(o32, dispatch),
    ...toJs(5, hi),
  ];
  b.exportFunc("o32", b.func([I32, I32, I32, I32, I32], [I32], [I64], code));
  b.exportFunc("run", b.func(...o32Entry(fns, hi, run)));
  return b.bytes();
}
