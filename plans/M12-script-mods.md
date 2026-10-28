# M12 — Script mods

**Status:** in progress (phases 1–3 built; the cost re-measurement and `psxw ci btn` remain)
**Depends on:** M11 (done). **Unlocks:** —
**Design refs:** design §7 (one module), §9 (debug builds), "One-shot
host scripting"; decisions D45 (non-local exits), D49 (hooks on function
boundaries), D51 (the moddable base), D52 (queued guest calls), D53
(DWARF), D54 and D55 (this plan).

## Goal

People mod a game in JavaScript, in the browser, with no build step:
- hook any hookable function by name;
- add commands they call from the console;
- read and write memory through the game's own types.

Editing a file or typing at the console takes effect at the next vblank.

Script mods sit beside M11's compiled mods; they don't replace them.
- **Both kinds coexist:** they load into the same moddable base (D51)
  and share its hook chains, load order and conflict rules (D49), so they
  can be applied together.
- **Compiled mods remain** for the native host and for hot inner loops.

Mods are typed from a decomp, but no mod depends on one decomp or on one
iteration of it. Names and types come from snapshots keyed by the game
binary (D55), so mods written against different decomp commits, or
different decomps, load together.

- **Phase 1:**
  - script hooks, commands and data patches on the moddable base;
  - the runtime holds the host's vblank wait while JS is on the stack;
  - loading, reloading and the console;
  - tests in headless Chromium and Firefox.
  
  Phase 1 uses names only.
- **Phase 2 (types):**
  - the snapshot format;
  - the symbol-list and C-header frontends;
  - a typed `game` object and a generated `game.d.ts`.
- **Phase 3 (iterations and sources):**
  - a snapshot per mod and `--bump` with aliases;
  - the DWARF frontend;
  - several sources merged;
  - packed mods.

Nothing in the modkit is specific to a game. The API examples below use
PsyQ functions (`rand`) or made-up game names (`AddGold`, `gGold`).
Per-game test mods live under `games/<id>/mods/`.

## Measured before planning

On 2026-10-27, wasm→JS call costs were measured in V8 13.6 (node 24,
`--experimental-wasm-jspi`). The calls ran on a JSPI stack, as the game
does, in a tight loop with empty hook bodies. Firefox wasn't measured.

| Per call | ns |
|---|---|
| wasm → wasm call | 0.2 |
| The moddable base's slot check, slot empty | 0.8 |
| Slot → a compiled mod's layer | 1.4 |
| Slot → wasm shim → JS hook | ~16 |
| JS hook returning an i64 (a `BigInt`) | ~22 |
| JS hook that calls back into wasm | ~22 |

- **Cost scales with calls.** A JS hook costs in proportion to how often
  its target is called. 10,000 calls a frame take about 0.15 ms of a
  16.7 ms frame.
- **JSPI can't suspend through a JS frame.** A suspend with a JS frame
  between two wasm frames fails with `SuspendError: trying to suspend JS
  frames`.
  - The runtime calls the host's `wait_vblank`, the one suspending
    import, from every delivered vblank (`psx-bios` `deliver`), and
    vblanks are delivered at any safepoint.
  - So any game code a JS hook calls can reach it. The runtime section
    below handles this.
- **Both hosts key replay input on `wait_vblank`.** They set their frame
  counter inside it (`web/worker.js`, `psxw-host-native`), and `pad_state`
  reads that counter for replay input. Delaying the call would therefore
  change the game's input. That's why ABI 10 splits the host's vblank.

## The API

### A script mod

```
gold-rush/
  mod.js            the mod: one ES module, no build
  tests/
    rush.json       the test format of compiled mods (M11 task 10)
```

```js
// mods/gold-rush/mod.js
// @ts-check
export default mod(({ game, mem, hook, log }) => {
  let rush = false;

  /** A new opcode for the game's script interpreter (o32 words in, v0 out). */
  const goldOpcode = (script, arg) => {
    game.AddGold(arg, 0);
    return 0;
  };

  return {
    id: "gold-rush",
    version: "0.1.0",
    load_after: ["other-mod"],

    hooks: [
      hook(game.AddGold, {
        // Typed from `void AddGold(s32 amount, s32 reason)`.
        around(original, amount, reason) {
          original(rush ? amount * 10 : amount, reason);
        },
      }),
      hook(game.points.frame, {
        after() {
          if (rush && game.gGold < 1000) game.gGold = 1000;
        },
      }),
      hook(game.rand, {
        before(limit) {
          log(`rand(${limit})`);
        },
      }),
    ],

    data: [
      { target: game.addr.gEnemyStats, offset: 4, type: "u16", value: 10 },
      { target: game.addr.gOpcodeHandlers, offset: 328, type: "fnptr", value: goldOpcode },
    ],

    commands: {
      /** Ten times the gold (nonzero), or normal; returns the new state. */
      GoldRush(on) {
        rush = !!on;
        return rush;
      },
    },
  };
});
```

**The default export** is `mod(factory)`.
- **Load:** on each load, the loader calls the factory with the mod's
  own `game` (D55). It reads the returned definition before installing
  anything, so every hook's target and kind, every data patch and every
  command are known before any of them runs.
- **State** belongs inside the factory, so a reload starts fresh.
- **Globals:** `mod` and the API types are globals in the game Worker,
  declared by the SDK's `psx.d.ts`.

**Fields:**
- `id`, `version` and `load_after`, as in `mod.json` (M11);
- `hooks`, `data` and `commands`;
- `init()`, optional: it runs once the mod is installed and the game has
  booted, like compiled mods' `psxmod_init`: at boot for a mod loaded
  before it, at load (through the queued call, so inside the game)
  otherwise.

### Hooks

`hook(target, handlers)` declares hooks on one target. A target is a
function from `game`:
- `game.Name`;
- `game.points.<point>`, a point from the manifest's `[mod.points]`;
- `game.at(container, address)`, for a function with no name.

One handlers object may hold several kinds, and one object may be passed
to several targets.

| Handler | Receives | Can | M11 kind |
|---|---|---|---|
| `before(...args)` | the arguments | observe; write memory; call game functions | `before` |
| `after(result, ...args)` | the result (`undefined` for a `void` target), then the arguments as passed | observe | `after` |
| `replace(...args)` | the arguments | return the result; nothing below it runs | `patch` |
| `around(original, ...args)` | the next link, then the arguments | call `original` any number of times, with any arguments; return the result | `patch` with `original` |

- **Composition, conflicts and which calls fire are M11's.**
  - A script mod adds one more layer to each target's chain (D51), in
    load order with compiled mods.
  - Two `replace`s on one target warn, and the last applied wins.
  - A `replace` makes an earlier mod's `before`/`after` on that target
    unreachable.
  - Every call through the target's hook slot fires the hook: direct C
    calls, translated calls, the dispatch table and overlay stubs.
- **`from` isn't available.** D51: mods loaded at run time can't tell
  callers apart.
- **Arguments are numbers.**
  - From phase 2, a handler gets the target's declared parameters,
    converted from its slot: a C-typed slot's wasm values, or an o32
    slot's argument words and stack.
  - Before phase 2, or without a declaration, it gets the raw values: an
    o32 slot's `a0`–`a3`, or a C slot's wasm values.
  - 64-bit values are `bigint`.
- **A handler that throws** is logged, and its mod is disabled at the
  next vblank. The call it was handling passes to the next link, as if
  the layer weren't there.
- **Game calls happen only inside the game:** in handlers, commands and
  `psx.run`. Memory reads and writes work anywhere; at the console they
  see the game between frames.

### Commands

`commands` are functions the console calls by name.
- **Where they run:** inside the game, at the next safe boundary, through
  D52's queued call, so they can call game functions.
- **What the console gets:** a Promise of the command's result.
- **One namespace:** compiled mods' global functions and script mods'
  commands share it, so `wasmPsxScripting.execute("GoldRush", 1)` works
  for either kind.

### What a mod sees

- **`game`.** Phase 1 provides functions, `addr`, `points` and `at`;
  phase 2 adds the typed parts.
  - Functions: `game.AddGold(10, 0)` calls one; `game.AddGold` is also a
    hook target.
  - Globals are properties. `game.gGold` reads and writes a scalar at its
    declared width and sign. Arrays and records are views:
    `game.gPlayer.hp`, `game.gEnemyStats[3].attack`.
  - `game.addr.<global>` is a global's address.
  - `game.types.<Record>.at(address)` is a view of a record at any
    address.
  - `game.points.<point>` and `game.at(container, address)` are hook
    targets.
- **`mem`:**
  - reads: `mem.u8`, `s8`, `u16`, `s16`, `u32`, `s32(address)`;
  - writes: `mem.setU8` … `mem.setS32(address, value)`;
  - `mem.bytes(address, n)`.

  Addresses are guest addresses, masked the way the game's are.
- **`psx.struct(fields)`** defines a record type of the mod's own, for
  memory no decomp has typed:
  `psx.struct({ hp: { type: "s16", offset: 0x40 } }).at(base)`.
- **`log(...)`** writes a line to the mod's log. M11's `log` expectation
  reads it.

### Types for the editor

`psxw mod sdk` writes two files:
- `.psxw/sdk/game.d.ts`, generated from the workspace's snapshot
  (phase 2);
- `.psxw/sdk/psx.d.ts`, the API.

`psxw mod new --script` writes a `jsconfig.json` that turns on `checkJs`
over `mods/` with both files. Mods stay plain JavaScript, and the editor
checks them:

```ts
// .psxw/sdk/game.d.ts (generated; excerpt)
interface Game {
  /** `void AddGold(s32 amount, s32 reason)` · economy.h · exe 0x80012340 · c */
  AddGold: GameFn<(amount: s32, reason: s32) => void>;
  /** `extern s32 gGold` · economy.h · exe 0x8007A000 */
  gGold: s32;
  addr: { readonly gGold: number /* … */ };
  types: { Player: RecordType<Player> /* … */ };
}
interface Player extends GameRecord {
  /** +0x00 */ flags: s32;
  /** +0x40 */ hp: s16;
}
```

`hook()` is generic over its target:
- each handler's parameters and result are the target's;
- a misspelled target is an error.

`s32` and the other width types are aliases of `number`: they document a
value's width but don't check its range.

### At the console

In DevTools, with the console's context set to the game Worker:

```js
await psx.cmd("GoldRush", 1)        // runs in the game at the next boundary
psx.reload("gold-rush")             // re-imports mod.js; psxw mod watch does this on save
psx.mods()                          // the loaded mods of both kinds, in order, with their chains

// Without writing a mod:
const h = psx.hook(game.rand, { after(r, limit) { console.log(`rand(${limit}) = ${r}`); } });
h.remove();
game.gGold                                  // memory reads work any time
await psx.run(() => game.AddGold(100, 0))   // game calls run in the game
```

For use from the page's own context, `wasmPsxScripting` gains
`cmd(name, ...args)` and `load(url)`.

## The runtime: holding the host's vblank wait

D54 records this design.

- **ABI 10 splits the host's vblank in two:**
  - **`host.vblank(n)`** never suspends. It runs where `wait_vblank` runs
    now, at every delivered vblank. Everything a host does that the guest
    can see moves here:
    - the frame counter that replay input is keyed on;
    - swaps and loads scheduled for a vblank;
    - queued calls (`--execute`, test commands).
  - **`host.wait_vblank(n)`** may suspend. It only paces, presents and
    yields to the event loop; nothing in it reaches the guest.
- **`psxw_js_enter()` / `psxw_js_leave()`** bracket every call from a
  shim into JS.
  - While the count is nonzero, the runtime holds `wait_vblank` calls.
  - When the count returns to zero, the runtime makes the held calls in
    order, before returning to the shim. At that point only wasm frames
    are left on the stack, and the machine isn't borrowed.
- **The guest can't see the hold, by construction.** Everything the
  guest can see happens in `host.vblank`, at its usual instant.
  - Only the page's pacing slips while JS is on the stack.
  - An `around` that wraps several frames of waiting, such as a loading
    loop, runs those frames unpaced, and the picture holds until it
    returns.
- **The native host** never enters the hold, since it runs no JS. Its
  per-vblank work (the frame counter, swaps, `--execute`) moves into
  `host.vblank`.

## Phase 1: script hooks

### Tasks

1. [x] **Runtime and ABI 10:**
   - add `host.vblank` and `host.wait_vblank` as above, and
     `psxw_js_enter`/`psxw_js_leave`;
   - both hosts move their guest-visible per-vblank work into
     `host.vblank`;
   - a driver test: no held wait is made while the count is nonzero, and
     the held waits are made in order once it returns to zero;
   - the runtime counts held waits and reports them in its run report.
2. [x] **Shim generator** (`web/shims.js`). JS writes each shim's wasm
   bytes itself, from the slot's flavour (`c <params>-><results>` or
   `o32`, from `psxmod.slots`) and the layer's handler kinds.
   - **Imports:** the base's chain table, `psxw_js_enter`/`leave`, and one
     JS function per handler.
   - **Exports:** `hook.<k>` for each hooked slot, in the slot's own type;
     `psxmod_init` and `psxmod_loaded`, for the mod's data patches and
     `init`.
   - **Kinds:**
     - for `before` and `after`, the shim calls JS around the next link
       and calls the next link itself;
     - for `replace`, it returns JS's result;
     - for `around`, it hands JS a function that calls the next link.
   - **i64 values** cross as two i32 words, which the shim splits and
     reassembles, so no `BigInt` crosses per call.
   - **Balance:** a wasm exception that unwinds through the shim (D45's
     interrupt exits, or a throwing hook) still runs `psxw_js_leave`.
   - **Synthetic addresses:** commands, `psx.run` callbacks and `fnptr`
     values get o32 entries in the dispatch table, at synthetic addresses
     from the mod's range, as compiled mods' functions do (`web/mods.js`).
3. [x] **Loader** (`web/mods.js`).
   - **One list:** a script mod is an entry in `ModLoader.mods`, with the
     same shape as a wasm mod (slots, originals, exports), so `rewire()`
     chains both kinds in load order.
   - **Errors:** each target resolves to its slot. The loader refuses an
     unknown target, a target without a slot, and a `replace` of a
     runtime adapter.
   - **Warnings:** conflicts warn in the `mods` message, naming both mods
     and the winner.
   - **Throwing handlers** behave as described in The API.
4. [x] **Names in the bundle.** Every moddable bundle carries a
   names-only snapshot: the symbol-list frontend's output, in D55's
   format.
   - **Contents:** functions and globals by container and address, with
     their names, and `[mod.points]`. Phase 2 adds types to the same
     format.
   - **The phase 1 `game`,** built from it: functions (callable, and hook
     targets), `addr`, `points` and `at`.
   - **Splat auto-names** (`func_<address>`, `D_<address>`) resolve by
     their address, as the address map resolves them (`psxw-inputs`
     `auto_name`). So `game.addr.D_8007AFF0` works whether or not a
     symbol file lists the name.
5. [x] **Calls from JS into the game.**
   - `game.F(...)` calls the head of F's chain: `psxmod_entries` for a
     C-typed slot, the dispatch table for an o32 one.
   - Calls work only inside the game, which the shims track with a flag.
     Outside it, a call throws, naming `psx.run`.
6. [x] **Commands and `psx.run`.**
   - They run through `psxw_execute` (D52), at their synthetic addresses;
     each result resolves the console's Promise.
   - The Worker lists commands together with compiled mods' functions, so
     `execute(name)` finds either kind.
7. [x] **Data patches** (`data`), with M11's semantics:
   - EXE data is patched after boot;
   - overlay data is patched when the overlay becomes resident
     (`psxmod_loaded`);
   - `fnptr` values are JS functions.
8. [x] **Loading and reloading.**
   - **Sources:** script mods arrive by URL (`?mods=` takes `.js` too),
     from the mod panel (single-file mods in OPFS), and from
     `psxw mod watch`.
   - **Watch:** `watch.json` carries each publish's kind and a versioned
     URL (`mods/<id>@<seq>/mod.js`), so a mod's relative imports reload
     with it.
   - **When:** imports finish during a suspension, and the mod is
     installed at the next `host.vblank`.
   - **Reload** replaces the mod in place, at its position, and runs its
     factory again.
9. [x] **Console:**
   - `psx` in the Worker: `hook`, `cmd`, `run`, `load`, `reload`,
     `unload`, `mods`;
   - `wasmPsxScripting.cmd` and `load` on the page.
10. [x] **Workspace commands:**
    - `psxw mod new <id> --script` writes `mod.js`, `tests/` and
      `jsconfig.json`.
    - `check` reads script mods' definitions by running their factories
      against a stub API in the browsers image (D44). It then resolves
      them together with compiled mods into one chain table.
    - `build` builds compiled mods and the moddable base.
    - `play` and `watch` serve both kinds.
    - `test` runs script mods' tests in the browsers.
11. [x] **Tests in the browser.** `web-test game` gains what
    `psxw mod test` needs: per-vblank traces, `load_at`, commands at
    vblanks and mod logs. Results come back in the native JSON shape, so
    `expect` and `check` run unchanged. The test schema gains:
    - `commands: [{ "at", "name", "args" }]`, for either kind;
    - an `offset` on trace entries;
    - an `expect` kind `same_as: { "mod" }`: the run's RAM, VRAM and
      audio hashes equal a run with that mod instead.
12. [x] **Test mods and the gate:**
    - script twins (`<id>-js`) of the compiled test mods in
      `games/<id>/mods/`;
    - a generated script pass-through: `before`, `after` and an `around`
      that calls `original`, on every hookable function;
    - the `mods` gate runs them in Chromium and Firefox.
13. [x] **Cost.** Measure frame time in Chromium and Firefox over
    `title-to-gameplay` with `web-test pacing`, for:
    - the plain bundle;
    - the moddable base (the number M11's exit criteria still owe);
    - the base with a script hook on a frequently called function, and
      on a once-a-frame one.

    Record the results in Findings.
14. [x] **Docs:**
    - `web/README.md`: script mods and the console;
    - `docs/design.md`: a "Script mods" section. The scripting section
      stops calling itself "not a script loader";
    - the mods repo README.

### Exit criteria (all automated)

1. `psxw ci <game>` passes, including the `mods` gate's browser part.
2. **ABI 10 changes nothing without JS:** the existing replays' RAM, VRAM
   and audio hashes are unchanged, natively and in both browsers.
3. **The guest can't see the hold:**
   - the script pass-through on every hookable function leaves
     `title-to-gameplay` bit-identical to the plain build, in Chromium
     and Firefox;
   - the run report shows waits held while JS was on the stack.
4. **Backends agree:** each script twin's run equals its compiled mod's
   run (loaded at run time) bit for bit over its replay, in both
   browsers, with any commands at the same vblanks.
5. **Reload is transparent:** reloading an identical script mod
   mid-replay leaves the run bit-identical, and a changed one shows its
   new behaviour from the next call.
6. **Conflicts** (a test): two script `replace`s on one target, and a
   script `replace` over a compiled mod's `patch`, each warn naming both
   mods, and the last applied runs.
7. **Cost is recorded** in Findings.

## Phase 2: types

### Tasks

1. [x] **The snapshot format** (D55;
   `psxw-modkit/schema/snapshot.schema.json`). A snapshot is one source's
   view of one game binary:
   - `game`: the identity hashes, as `identity.json` records them;
   - `source`: kind and version (for example a decomp commit);
   - `functions` and `globals`: by container and address, with names and
     a signature or type;
   - `types`:
     - records (struct, union): size, alignment, and fields (name,
       offset, bit offset and width, type);
     - enums and typedefs;
   - type expressions, as data: a width (`u8` … `s64`, `f32`, `f64`),
     `{ "ptr": T }`, `{ "array": T, "count": n }`, `{ "record": id }`,
     `{ "fn": signature }`;
   - `points`, from the manifest.

   Headers and a decomp's conventions appear in a snapshot only as
   provenance.
2. [x] **Symbol-list frontend:** names, addresses and sizes from the
   address map (`psxw-inputs`). Every game gets at least this much.
3. [x] **C-header frontend:**
   - **Declarations:** the header index's declarations, parsed into type
     expressions with tree-sitter (`psxw_cc::ast`), each in its declaring
     header's include context. `__asm__` labels name globals.
   - **Fields:** each record's fields and their types, from
     `ast::records()`.
   - **Layout:** sizes and offsets from the layout probe
     (`psxw_cc::probe`): clang wasm32, compared with the manifest's
     reference compiler where it has one. Fields the two compilers
     disagree on are dropped and listed.
   - **Bitfields** expose the word that holds them, until phase 3's DWARF
     frontend.
   - **Declared types stand.** A global declared as an array behind a
     scalar `#define` keeps its declared array type. The census's fixups
     use that pattern (`extern s32 X[3]; #define X (X[0])`).
4. [x] **The typed `game`,** built from a snapshot:
   - globals as typed properties and views;
   - records through `game.types`;
   - pointers as numbers, viewed with `game.types.T.at(p)`;
   - handlers' parameters and results converted from the slot by the
     declared signature. An o32 slot's words and stack follow the o32
     rules that psxw-link's `marshal` applies;
   - `psx.struct`.
5. [x] **`game.d.ts` and `psx.d.ts`:**
   - `game.d.ts` is generated from a snapshot;
   - handlers are typed from their targets;
   - each symbol's doc comment gives its declaration, source, container,
     address and tier.
6. [x] **The SDK:**
   - `psxw mod sdk` writes the workspace's snapshot (named by its hash)
     and `game.d.ts`;
   - `psxmods.lock.json` records the snapshot's hash;
   - `play` and `watch` bundles carry each served mod's snapshot, and each
     mod's `game` comes from its own.
7. [x] **Type checking:** pinned Node and TypeScript in the browsers
   image; `psxw mod check` runs `tsc --noEmit` over script mods.
8. [ ] **Acceptance** in the mods repo (below).

### Exit criteria (all automated)

1. Phase 1's criteria still pass.
2. **The C frontend's snapshot agrees** with the census probe on every
   layout it holds, and with the address map on every address.
3. **Types are checked:** the acceptance mods type-check. A fixture fails
   with exactly three errors: a misspelled target, a wrong handler result
   type, and a write to a missing field.
4. **Acceptance** steps pass in Chromium and Firefox.

## Phase 3: iterations and sources

### Tasks

1. [x] **Snapshots per iteration:**
   - cached by source and version, and named by hash;
   - several can be loaded in one session; each mod's `game` comes from
     the snapshot its workspace pinned.
2. [x] **`psxw mod sdk --bump`** compares the old and new snapshots by
   address:
   - **renames:** a renamed function or global keeps its old name as an
     alias, in `game` and as `@deprecated` in `game.d.ts`, naming the new
     one;
   - **layout changes:** a record whose fields moved or changed type is
     reported at each anchor that reaches it (a global's type, a
     function's parameter);
   - **removals:** a name whose address no longer holds a function or
     global is an error naming what is there now (M11's rule).
3. [x] **DWARF frontend,** from `psxw build --debug` (D53):
   - types and layouts as compiled, one compilation unit at a time,
     bitfield offsets included;
   - merged with the C frontend: layouts from DWARF, and declarations of
     functions that have no C from the headers.
4. [x] **Several sources:**
   - `psxmods.json` lists sources in order of precedence;
   - their snapshots merge by address;
   - every source's names become aliases;
   - type disagreements warn, naming both sources and the winner;
   - `game.d.ts` shows each symbol's provenance.
5. [x] **Workspace-local declarations:** snapshot fragments in the mods
   repo (`types/*.json`), the most local source, for memory no decomp
   has typed.
6. [x] **Packed mods:** `psxw mod pack <id>` writes one file holding the
   mod and the part of its snapshot it uses; the mod panel accepts packs.

### Exit criteria (all automated)

1. **Iterations together.** Take two snapshots of one game from two
   decomp commits, with renames between them:
   - `--bump` reports the renames;
   - a mod using an old name runs, and its check warns;
   - mods pinned to each snapshot load together and pass their tests.
2. **Frontends agree:** the DWARF and C frontends agree on every record
   and global type both describe.
3. **Names alone are enough:** a mod with only a symbol-list snapshot
   loads and runs by name.
4. **Slot flavours:** the same script mod passes against two bases where
   one target's slot is C-typed in one and o32 in the other. The bases
   can come from decomp commits before and after the function became C,
   or from a base built with it at its translated tier.

## Acceptance: `btn-mods`, with script twins

The milestone is accepted on the first target game's mods repo,
`../btn-mods`, with script twins of its compiled mods. Game names appear
here because this repo is btn's.

1. **The oracle is committed:** btn-mods' compiled mods (better-rand,
   rotate-world, noclip, freeze-time) and their tests are committed, and
   `inputs.lock` pins that commit.
2. **The twins:** `mods/<id>-js/mod.js` for each of the four, and
   `sets/toys-js.json` with the three toys.
3. **better-rand-js** passes better-rand's `sequence.json`, with
   `check_sequence.py` unchanged, and its swap test, and is `same_as`
   better-rand.
4. **The toys:** rotate-world-js, noclip-js and freeze-time-js are each
   `same_as` their compiled mod over `title-to-gameplay`. In both runs
   the mod's command (`RotateWorld 1`, `SetNoclip 1` or `FreezeTime 1`)
   runs at the same vblank.
5. **Types:** all four type-check against the generated `game.d.ts`.
6. **Reload:** phase 1's reload test passes on rotate-world-js.

The twins, as they would look:

```js
// mods/better-rand-js/mod.js
// @ts-check
const SEED = 0x2545F491;

export default mod(({ game, hook }) => ({
  id: "better-rand-js",
  version: "0.1.0",
  hooks: [
    hook(game.Rand, {
      // Typed from `u32 Rand(s32 limit)`.
      replace(limit) {
        let x = game.gRandomState || SEED;   // xorshift never leaves 0
        x ^= x << 13;
        x ^= x >>> 17;
        x ^= x << 5;
        game.gRandomState = x;               // stored as a u32
        return ((x >>> 16) * (limit & 0xFFFF)) >>> 16;
      },
    }),
  ],
}));
```

```js
// mods/noclip-js/mod.js
// @ts-check
const SKIP_PROBE = 2;   // the player's flag 2: the game skips its step probe

export default mod(({ game, hook }) => {
  const player = game.types.Ovl11PrimaryMotionObject.at(game.addr.gPrimaryObjectBase);
  let enabled = false;

  return {
    id: "noclip-js",
    version: "0.1.0",
    hooks: [
      hook(game.Ovl11UpdatePrimaryObjectMotion, {
        around(original) {
          const set = enabled && !(player.flags & SKIP_PROBE);   // flags: s32 at +0x6C
          if (set) player.flags |= SKIP_PROBE;
          original();
          if (set) player.flags &= ~SKIP_PROBE;
        },
      }),
    ],
    commands: {
      /** Walk through walls (nonzero) or not; returns the new state. */
      SetNoclip(on) {
        enabled = !!on;
        return enabled;
      },
    },
  };
});
```

```js
// mods/rotate-world-js/mod.js
// @ts-check
const QUARTER_TURN = 0x400;   // 4096 units a turn

export default mod(({ game, mem, hook }) => {
  // The scene overlays' camera yaw: a word in the untyped work area at D_8007AFF0.
  const yaw = game.addr.D_8007AFF0 + 0x253A0;
  let turns = 0;   // 0..3, kept across scenes

  const turn = (q) => mem.setS32(yaw, (mem.s32(yaw) + q * QUARTER_TURN) & 0xFFF);
  const keepTurns = { after() { turn(turns); } };

  return {
    id: "rotate-world-js",
    version: "0.1.0",
    hooks: [
      hook(game.Ovl11ResetSceneCameraOrbit, keepTurns),
      hook(game.Ovl19InitSceneCamera, keepTurns),
      hook(game.Ovl21InitSceneCamera, keepTurns),
      hook(game.Ovl25InitSceneCamera, keepTurns),
    ],
    commands: {
      /** A quarter turn: nonzero counterclockwise on screen, 0 clockwise; returns 0..3. */
      RotateWorld(direction) {
        const step = direction ? 1 : -1;
        turns = (turns + step) & 3;
        turn(step);
        return turns;
      },
    },
  };
});
```

```js
// mods/freeze-time-js/mod.js
// @ts-check
const TIME_STOPPED = 3;   // the game's own "time stopped" flag

export default mod(({ game, hook }) => {
  let frozen = false;

  return {
    id: "freeze-time-js",
    version: "0.1.0",
    hooks: [
      hook(game.Ovl11UpdateClockAndEvents, {
        // The flag is set only around the clock's update, so NPC schedules
        // and events never see it.
        around(original) {
          if (!frozen) return original();
          const was = game.GetGameFlag(TIME_STOPPED);
          game.SetGameFlag(TIME_STOPPED, 1);
          original();
          game.SetGameFlag(TIME_STOPPED, was);
        },
      }),
    ],
    commands: {
      /** Stop the clock (nonzero) or let it run; returns the new state. */
      FreezeTime(on) {
        frozen = !!on;
        return frozen;
      },
    },
  };
});
```

## Defaults taken

- **Twins:** script twins are separate mods named `<id>-js`, so both
  kinds sit in one repo and one set.
- **`after`'s arguments:** `after` gets the result first, for every
  target, `undefined` for a `void` one. Compiled `after` hooks on `void`
  targets take only the parameters.
- **Reload resets state:** reloading a script mod runs its factory again.
  A wasm swap keeps state when the layout matches.
- **Throwing handlers:** a throwing handler disables its mod, and the call
  passes to the next link.
- **No compile step:** mods are JavaScript with JSDoc and the generated
  `.d.ts`, checked by the editor and by `tsc` in CI. There's no
  TypeScript compile step.

## Not planned

- **Script mods in the native host.** wasmtime runs no JS, so script mods'
  tests run in headless browsers. Embedding a JS engine (QuickJS) would
  change that.
- **TypeScript sources.** They need a compile step, which this milestone
  exists to remove; `checkJs` gives the editor the same checks.
- **`from` hooks** (D51), memory watches and runtime events (D49).
- **Sandboxing.** Script mods are trusted code, as compiled mods are.
- **Enforcing determinism.** A mod that uses `Math.random` or `Date`
  breaks replays; the `deterministic` expectation catches it.
- **Mods across releases of a game** (NTSC, PAL): those are different
  binaries, with different addresses.

## Progress log

- **2026-10-27, phase 1** (`f591318`):
  - ABI 10: `host.vblank`/`host.wait_vblank`, `psxw_js_enter`/`leave`,
    the runtime's hold (`psx-runtime` `hold.rs`; a driver test), held
    waits in the run report. Both hosts moved their vblank work into
    `host.vblank`.
  - `web/shims.js` (each mod's layer, `next.<k>`, o32 entries, the host's
    call helpers), `web/script.js` (definitions, `game`, hooks, commands,
    data, console), `web/mods.js` (one list for both kinds, conflicts).
  - The base names each slot's tier (`psxmod.tiers`) and each overlay's
    resident words (`psxmod.resident`); moddable bundles carry
    `snapshot.json`.
  - `psxw mod test` runs a script mod's tests in both browsers (page
    runs); the test format gained `commands`, trace `offset` and
    `same_as`. `psxw mod check` runs factories in Node in the browsers
    image (now with Node 20.19 and TypeScript 4.9.5). `mod new --script`,
    `play`, `watch` (versioned URLs), `pass-through --script`.
  - Script twins of btn's five test mods: each passes its tests in
    Chromium and Firefox and is bit-identical to its compiled mod.
- **2026-10-27, phases 2 and 3** (`abd1ba6`): the snapshot format and
  schema, the C-header and DWARF frontends, the typed `game`, `game.d.ts`
  and `psx.d.ts`, type checking, `--bump` with aliases, sources and
  workspace fragments, packs, `Options::lift`, and the gate's script part
  (data-driven: the test format gained `swap`).
- **2026-10-27, acceptance** (btn-mods `792d8e7`, then the twins): the
  compiled toys got tests; better-rand-js passes `sequence.json` with
  `check_sequence.py` unchanged, and its swap test, and is `same_as`
  better-rand; the three toy twins are each `same_as` their compiled mod
  with their command at the same vblank; all four type-check.

## Findings

- **Cost** (task 13; 2026-10-27, `web-test pacing --replay
  title-to-gameplay`, 6000 vblanks paced at real time, this machine;
  frame time is the wall time between vblanks less the pacing sleep):

  | Bundle | Chromium p50 / p95 / p99 ms | Firefox p50 / p95 / p99 ms | Late frames (C / F) |
  |---|---|---|---|
  | Plain | 2.61 / 20.94 / 25.88 | 4.38 / 7.90 / 11.02 | 59 / 108 |
  | Moddable base, no mods | 2.54 / 21.53 / 26.71 | 4.06 / 7.30 / 9.52 | 52 / 86 |
  | Base, a no-op script `before` on `_padChkRC2wait` (~4,000 calls a vblank) | 3.01 / 21.81 / 30.54 | 4.82 / 8.18 / 11.50 | 159 / 97 |
  | Base, a no-op script `after` on the `frame` point (once a frame) | 2.43 / 20.20 / 24.89 | 4.30 / 7.38 / 9.48 | 49 / 98 |

  - The moddable base costs nothing measurable (the number M11's exit
    criteria owed), nor does a once-a-frame hook.
  - The hot hook adds about 0.4 ms a frame at the median in both
    browsers: about 100 ns a call, the 16 ns shim-to-JS crossing plus the
    wrapper (arguments, the bracket's two calls). Chromium fell behind
    more often (159 late frames, 33 frames of drift that the 250 ms
    resync absorbed).
  - Firefox ends about 15 frames behind wall time over the replay in
    every variant, the plain bundle included: its pacing, not M12's.
  - A first set of runs overlapped a compile; its outliers (a 1 s
    Firefox frame, 300 frames of Chromium drift) are discarded.
- **ABI 10 changes nothing without JS.** Natively, `title-to-gameplay`,
  `card-load`, `frozen-cutscene` and `start-no-cutscene-interaction` (6000
  vblanks each) give the same RAM and VRAM hashes at 1500/3000/4500/6000
  and the same audio under ABI 9 (`ed0e85a`, built in a worktree) and
  ABI 10; the browsers equal the native host over `title-to-gameplay`
  (`web-game`).
- **Exactness needs two rules for `around`.** Calling `original` with the
  arguments it was given passes the original words on (an o32 call's pad
  words, a value typed narrower than its register), and returning
  `undefined` passes the original's result on whole, `v1` included: some
  functions declared `void` have callers that read `v0` (the build's
  `void_results`). With both, a pass-through is bit-identical.
- **Hottest hookable function in btn:** libpad's `_padChkRC2wait`, about
  4,000 calls a vblank in gameplay; the next is `GsSetWorkBase` at about
  47 (counted by a script mod with a `before` on all 3,375 hookable
  functions, the same counts in both browsers). That run (3000 vblanks,
  unpaced) took 28 s in Chromium and 16 s in Firefox.
- **The C frontend on btn** (`b37ae28`): 3,387 functions (2,742 with
  signatures), 1,197 globals (1,025 typed), 839 records. 34 entries are
  dropped (layouts the two compilers can't both compute; PsyQ headers
  that need types their context doesn't include, such as `MATRIX` and
  `u_short`). Eight headers the reference compiler can't compile in their
  include context (the census skips them too) keep clang's layouts,
  listed as `unchecked`. Types a header uses that its context doesn't
  define (m2c's, which the decomp defines in C files; 32 in `ovl_11.h`)
  get opaque declarations, enough for their pointer uses.
- **Plain `char` is unsigned** in psx-wasm's C (`-funsigned-char`, as the
  reference compiler's flags). The first C frontend read it as signed;
  the DWARF frontend caught it (490 disagreements, all `char`).
- **A symbol several declarations view.** The decomp declares some
  globals under other C names with `__asm__` labels, at other types (three
  declarations of `gCalendarYear`). A snapshot types each symbol once: the
  declaration under its own name, else the first.
- **Decomp iterations.** Between btn's previous pin (`3a15992`) and the
  current one, 587 functions were renamed, every one from a splat
  auto-name to a real name, and none between real names. The iterations
  test uses one of them (`ovl_27_func_800B7EEC`, now
  `Ovl27DispatchTitleScenePhase`); a rename between real names would need
  a census at another commit.
- **Not every toy shows over the replay.** `title-to-gameplay` walks into
  nothing, and the clock doesn't run on the opening day, so noclip's and
  freeze-time's effects aren't visible; their twins are held to their
  compiled mods by `same_as` (the flag writes around each call included).
- **What the build added to the plan's API:**
  - a `psxmod.json` beside a mod's `mod.js` names the snapshot its `game`
    comes from; that is how a bundle serves each mod its own (phase 2
    task 6, phase 3 task 1);
  - the test format's `swap: [{ "at", "mod" }]` puts another mod (or the
    same one, re-imported) in the tested mod's place at a vblank, and a
    `same_as` run has no swaps: the reload tests are data, like the rest;
  - `game.at(container, name)` besides `game.at(container, address)`;
  - `psx.hook` returns `{ id, ready, remove }`; `wasmPsxScripting` also
    has `reload`;
  - without a declaration, an o32 target's `after` and `original` give
    `v0`.
- **Traces read any RAM.** A work area past the executable's bss (btn's
  `D_8007AFF0`) is in no container, so trace entries fall back to the
  address an auto-name spells.
