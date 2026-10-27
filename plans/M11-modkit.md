# M11 — Modkit

**Status:** not started
**Depends on:** M2 (phase 1); M5 (phase 2). **Unlocks:** —
**Design refs:** design §2 (function pointers), §3 (overlays), §4 (one
implementation per function), §7 (one module, hot-swappable runtime), §8
(the link and guest passes); decisions D2, D3, D49.

## Goal

People change and extend a game built by psx-wasm with their own code:
replace or hook any function by name, put their own functions into the
game's tables and callbacks, and patch data. Several mods apply at once.

Mods are statically analyzable. Every hook, its target and its type are
stated in a declaration file, so the build (or the loader) knows before
anything runs what each mod touches and where two mods collide.

- **Phase 1** links mods into `game.wasm`, written in C.
- **Phase 2** loads separately built wasm mods, in any language that
  compiles to wasm, in the native host and the browser, and swaps them
  while the game runs.

Both phases take the same mod sources and declarations; only the backend
differs.

Nothing in the modkit is specific to a game. Examples below use PsyQ SDK
functions every game links (`rand`, `VSync`) or made-up game names
(`AddGold`, `gGold`). Per-game facts (named hook points, example mods)
live under `games/<id>/`.

## The API

### A mod

```
better-rand/
  mod.json          identity, load order, and every declaration
  src/
    random.c
    gold.c
```

### Declarations

Hooks are stated only in `mod.json`. Mod code is ordinary functions and
nothing in it marks a hook. A source file may define any number of
functions; only those a declaration names become hooks, and they may be
`static` (the build exports them).

Every distinct aspect is its own field. No value packs several facts into
one string. A JSON Schema shipped with psx-wasm describes the file.

```json
{
  "id": "better-rand",
  "version": "0.1.0",
  "game": "<id>",
  "load_after": ["other-mod"],
  "hooks": [
    {
      "kind": "patch",
      "target": { "symbol": "rand" },
      "impl": { "file": "src/random.c", "function": "better_rand" }
    },
    {
      "kind": "patch",
      "target": { "symbol": "CheckCollision" },
      "impl": { "file": "src/noclip.c", "function": "check" },
      "original": "check_original"
    },
    {
      "kind": "before",
      "target": { "symbol": "VSync" },
      "impl": { "file": "src/timing.c", "function": "on_vsync" }
    },
    {
      "kind": "after",
      "target": { "symbol": "AddGold" },
      "impl": { "file": "src/gold.c", "function": "on_gold" }
    },
    {
      "kind": "before",
      "target": { "symbol": "rand" },
      "from": { "symbol": "SpawnEnemy" },
      "impl": { "file": "src/luck.c", "function": "on_rand" }
    },
    {
      "kind": "after",
      "target": { "point": "frame" },
      "impl": { "file": "src/hud.c", "function": "on_frame" }
    },
    {
      "kind": "patch",
      "target": { "container": "ovl_02", "address": "0x800C1C90" },
      "signature": { "returns": "void", "params": [] },
      "impl": { "file": "src/menu.c", "function": "menu_init" }
    }
  ],
  "data": [
    {
      "target": { "symbol": "gEnemyStats" },
      "offset": 4,
      "type": "u16",
      "value": 10
    },
    {
      "target": { "symbol": "gOpcodeHandlers" },
      "offset": 328,
      "type": "fnptr",
      "value": { "file": "src/opcodes.c", "function": "my_opcode" }
    }
  ]
}
```

**Fields:**
- **`game`:** a built mod also records the game's file hashes.
- **`load_after`:** ordering constraints; see Composition.
- **`target` and `from`:** exactly one of these forms:
  - `symbol`: a game symbol, including splat auto-names;
  - `container` + `symbol`: a name more than one overlay defines;
  - `container` + `address`: a function with no name;
  - `point`: a named hook point from the game manifest.

  Addresses are hex strings, since JSON has no hex numbers.
- **`signature`:** required when no header declares the target. Each
  parameter is `{ "name", "type" }`, and each type is one C type.

### Hooks

| `kind` | Implementation type | Can |
|---|---|---|
| `before` | `void (params)` | observe calls; write through pointer arguments |
| `after` | `void (R result, params)`; `void (params)` for a `void` target | observe the result |
| `patch` | `R (params)` | replace the function |
| `patch` with `original` | `R (params)`; calls `original` | wrap it with full control: skip, change arguments, change the result |

**Modifiers and data:**
- **`from`** limits any hook to calls made inside one caller.
- **A `data` entry with `"type": "fnptr"`** stores a mod function's
  address in game data.

**`before` and `after` are a `patch` with `original` that the build
writes:**

```c
R F_hooked(params) {
    before(params);
    R r = original(params);   /* the rest of the chain, then the game's F */
    after(r, params);
    return r;
}
```

They can't skip `F`, change the arguments it gets, or change its result,
so they never change control flow. Several mods' `before`/`after` hooks on
one target never conflict; load order only orders their side effects.

**What they see.** The hooks run outside `F`: they see its arguments and
its result, not its locals. `after` sees the arguments as passed even if
`F` reassigned its parameters. Memory reached through a pointer argument
is game memory, so `after` sees what `F` wrote there. An exit by
`longjmp` skips `after`.

**Composition.** Mods are applied in load order: `load_after`
constraints first, then the order the user lists them. Each applied mod
wraps what is already there, so the last applied mod is outermost:

```
entry(F)   = layer(last applied) ∘ … ∘ layer(first applied) ∘ F's own implementation
layer(mod) = { its befores; its patch, or the next layer; its afters }
```

**Conflicts warn, and the last applied mod wins.** They are never errors:

| Conflict | Outcome |
|---|---|
| Two full replacements of one target (a `patch` without `original`), in overlapping `from` scopes | The last applied runs; the earlier is unreachable. |
| A full replacement applied over an earlier mod's `before`/`after` on that target | Those hooks are unreachable. |
| Overlapping data patches | The last applied value is written. |

Each warning names both mods, the target, and which one wins. All of them
go into `mods.json`.

**Errors** are reserved for a mod's own defects, and fail the build:
- a target that doesn't exist;
- a kind that doesn't fit its target;
- an implementation missing from its file, or with the wrong signature;
- a data patch over code.

**Which calls fire a hook.** A hook fires on every call the original
program makes to the target's address:
- direct calls from decompiled C;
- calls from translated code, which bind by address (`psxw_f_<ADDR>`);
- calls through function pointers, via the dispatch table;
- overlay dispatch stubs;
- recursion and tail calls.

A jump into the middle of a function (an interior entry) is not a call
and fires nothing, as on the hardware.

**Inlining.** psxw-cc compiles at `-O2`, so clang inlines calls between
functions in one translation unit. GCC 2.95 at `-O2` does not inline
functions that aren't declared `inline`, so those calls exist in the
original program. The build therefore recompiles each translation unit
that defines a hooked target with the target marked `noinline`. For
`from`, the caller's translation unit is recompiled the same way.
`wasm-opt` runs after the link, when every call already goes to the
generated entry, so its inlining keeps the hooks.

### What mod code sees

- **The game's headers.** Mod sources include the decomp's own headers.
  `psxw mod build` compiles them with the include paths and defines the
  game's C uses (`[c]` in `games/<id>/game.toml`).
- **Game globals** are undefined symbols in the mod's objects. The build
  resolves them to their original addresses through the address map, and
  accesses are translated as for game code.
- **Calls to game functions** go to the head of that function's hook
  chain, the same entry the game's own callers reach. Inside a hook on
  `F`, call `original`, not `F`: calling `F` runs the chain again,
  including the hook itself.
- **Mod state.** The mod's globals, stack and heap live in its own area
  of the guest window (in the auxiliary region above RAM's mirror), never
  in the game's 2 MiB of RAM.
- **Mod function addresses.** A mod function whose address is taken, or
  that a `fnptr` patch names, gets a synthetic MIPS address, as
  compiler helpers already do. The game calls it through the dispatch
  table like any function pointer. Mod code can also store one directly
  (`gOpcodeHandlers[82] = my_opcode;`), for example in an `after` hook on
  the function that loads or initializes an overlay.

### Generated files

`psxw mod build` writes these into the mod's build directory:

- **`mod.h`.** Declarations for the names the build provides, and
  prototypes that make clang check every hook's type. A definition that
  disagrees fails with `conflicting types`.

  ```c
  /* mod.h: generated from mod.json by psxw mod build. Do not edit. */
  #pragma once
  #include "libc.h"             /* declares rand */
  #include "collision.h"        /* declares CheckCollision */
  #include "economy.h"          /* declares AddGold */

  /* patch rand <- src/random.c: better_rand */
  __typeof__(rand) better_rand;

  /* patch CheckCollision <- src/noclip.c: check */
  __typeof__(CheckCollision) check;
  __typeof__(CheckCollision) check_original;    /* the next link in the chain */

  /* after AddGold <- src/gold.c: on_gold */
  void on_gold(int result, int amount, int reason);
  ```

  - **`patch` prototypes** copy the target's type with `__typeof__`, so the
    generator never spells it out.
  - **`before`/`after` prototypes** differ from the target's type, so
    their parameter types are spelled from clang's AST of the declaring
    header, which records each type as written.
  - **The declaring header** of each target comes from a header index
    built from the census's header parsing.
- **`compile_commands.json`,** so clangd resolves the decomp's headers and
  `mod.h`. Go-to-definition on a game function opens the decomp's
  declaration.

### Named hook points

Some points are wanted by many mods: "once per frame, after input is
read", "at boot". A PS1 game has no generic frame. The hardware has
vblank, which arrives as an interrupt in the middle of game logic, and
each game defines its own frame through its main loop. So the modkit has
no runtime events. A game's manifest can name real function boundaries:

```toml
# games/<id>/game.toml
[mod.points]
frame = { after = "ReadInput" }   # the main loop calls it once per frame
boot  = { before = "main" }
```

A hook with `"target": { "point": "frame" }` resolves statically to that
boundary.

### Static checks

From the declarations alone (`psxw mod check <game> <mods…>`, no
compiling):
- every target exists, and an overlay name that needs its container has
  one;
- each kind fits its target. A runtime adapter can be wrapped (`before`,
  `after`, `patch` with `original`) but not fully replaced. A function the
  translator expands into its callers can't be hooked;
- data patches land in data ranges, never code;
- across mods: the conflicts above, each with its winner, and the chain
  table for every hooked target.

From the compiled objects:
- each `impl` is defined in the file the declaration names;
- each implementation's signature agrees with its target's (the
  linker's existing C-level comparison);
- each mod's game surface, from relocations: the game functions it calls
  and the globals it reads and writes. Two mods writing the same global
  get a warning.

The build writes all of it to `mods.json`: chains, owners, warnings,
surfaces.

### A built mod (phase 2)

A built mod is a wasm module:

**Custom section `psxmod.decl`.** It holds:
- the declarations, resolved to container, address, kind, implementation
  export and signature;
- the game's identity: the hashes of the EXE and of every overlay;
- the mod's own hash.

Built mods record addresses, so they keep working when a decomp renames
things.

**Exports:** one per declaration.

**Imports:**

| Import | What it is |
|---|---|
| `psx.memory` | the guest memory, shared |
| `game.<name>` | the head of that game function's chain |
| `original.<name>` | the next link below this mod |
| `psxmod.*` | the mod library (log, config values) |

The loader refuses a module whose exports don't match its declarations,
or whose game identity doesn't match the running game.

## Phase 1: static mods

### Tasks

1. [ ] **`psxw-modkit` crate:**
   - the JSON Schema for `mod.json`;
   - parse and validate `mod.json`;
   - resolve targets through the address map and `[mod.points]`;
   - the declaration-only checks;
   - `psxw mod check <game> <mods…>` prints the chain table and the
     warnings, and exits non-zero only on errors.
2. [ ] **Header index:** for each symbol, the header that declares it and
   its parameter types as spelled (clang's AST over the headers the census
   already parses). Generate `mod.h` and `compile_commands.json`.
3. [ ] **Authoring commands:**
   - `psxw mod new <game> <id>` scaffolds a mod.
   - `psxw mod show <game> <symbol>` prints the declaration and where it
     is defined in the decomp, the function's tier and its callers.
4. [ ] **Compile mod sources** with psxw-cc: the game's flags plus the
   mod's include directory. Check that each `impl` is defined where
   declared and that its signature agrees with the target's.
5. [ ] **`noinline` rebuilds:** recompile each translation unit that
   defines a hooked target, and each `from` caller's, with the target
   `noinline`. Cache by the set of `noinline` names.
6. [ ] **Linker:**
   - one generated entry per hooked (container, address);
   - rebind to it every direct C call, translated call, dispatch-table
     entry and overlay-stub candidate;
   - `from` rebinding per call site;
   - synthetic addresses for mod functions;
   - mod data in a mod area of the window, with its range fixed in
     `psxw-abi`;
   - mod references to game globals resolved through the address map.
7. [ ] **Data patches** to EXE data, including `fnptr`, applied after
   boot loads the EXE. Overlay data is task 11.
8. [ ] **Running mods:** `psxw run <game> --mod <dir>…` and
   `psxw build-web <game> --mods <dir>,…`; write `mods.json`.
9. [ ] **Example mods** for the first target game, under
   `games/<id>/mods/`:
   - a full replacement of an SDK function (`rand`);
   - a `patch` with `original`;
   - an `after` hook that reacts to a value;
   - a `fnptr` patch.

   Each comes with a replay and a RAM-by-symbol check that shows its
   effect.
10. [ ] **CI gate `mods`:** builds the examples, runs their replays and
    checks their effects. It also checks that an empty mod set produces a
    `game.wasm` identical to the build without mods.
11. [ ] **Overlay data patches:** apply them when the overlay becomes
    resident. Design the residency signal (a CD transfer completing over
    the overlay's range, then the stubs' comparison of the overlay's
    original words) and record overlays that arrive another way
    (`memcpy`, decompression) in Findings.
12. [ ] **`psxw mod writers <game> <symbol>`:** lists the functions that
    store to a symbol's address, to find what to hook. It reports the
    functions where it lost track (stores through computed pointers)
    rather than implying a complete list.

### Exit criteria (all automated)

1. `psxw ci <game>` passes, including the `mods` gate.
2. With no mods, the `game.wasm` produced through the modkit path is
   byte-identical to the plain build.
3. Every example mod's replay shows its effect, and a two-mod set runs
   with the chain table `mods.json` reports.
4. Two full replacements of one target make `psxw mod check` warn,
   naming both mods, and the last applied replacement is the one that
   runs (a test).

## Phase 2: dynamic mods

### Tasks

1. [ ] **Post-pass for any wasm module:**
   - mask its loads and stores, as the guest pass does for game code;
   - move its data, stack and heap into the mod's area;
   - refuse or redirect `memory.grow`.

   First languages: C through plain clang, and `no_std` Rust with a small
   `psx-mod` crate that provides an allocator over the area.
2. [ ] **The built module:** `psxw mod build` emits it with its
   `psxmod.decl` section, one export per declaration, and its imports.
3. [ ] **Moddable base build:** each hookable entry checks a hook slot.
   Measure the cost on the first target game over `title-to-gameplay`
   (frame time). If it's too high, record it and restrict slots to
   targets declared at base-build time.
4. [ ] **Loader** in the runtime and both hosts:
   - verify identity and declarations;
   - resolve names;
   - wire chains wasm-to-wasm, passing instance exports and a shared
     function table directly as imports. A JS frame between two wasm
     instances would stop JSPI from suspending, as in `?dev` today.
5. [ ] **Native host `--mod <file>.wasm`.**
6. [ ] **Browser mod panel:** add a mod from a file, store it in OPFS,
   enable, disable and reorder; mods load at boot.
7. [ ] **Hot swap at vblank,** while the game waits in `wait_vblank`:
   - rewire the hook slots; frames already on the stack finish in the old
     code;
   - keep mod state when its layout hash matches, otherwise reset it;
   - `psxw mod watch <dir>` rebuilds on save and pushes the module through
     `psxw serve`.
8. [ ] **Rust bindings** over the decomp's headers (bindgen), with the
   Rust counterpart of `mod.h`.

### Exit criteria (all automated)

1. **Backends agree:** the example mods built statically and loaded
   dynamically give identical RAM, VRAM and audio hashes over their
   replays, natively and in Chromium and Firefox.
2. **Swapping is transparent:** swapping a mod for the identical module
   mid-replay leaves the run bit-identical to the run without the swap.
   Swapping in a changed module shows the new behaviour from the next
   call on.
3. **Cost is recorded:** the moddable build's frame-time cost is in
   Findings.

## Not planned

- **Memory or value watches.** One would mean instrumenting every store
  the game makes. A mod reacts to a value with ordinary hooks: an `after`
  on the function that changes it, or a check in an `after` on a point
  that runs often enough.
- **Runtime events.** Named hook points cover them with real function
  boundaries.
- **Hooks inside a function**, at an original instruction's address. They
  could only work for translated functions, and would stop working once
  the decomp writes that function in C.

## Progress log

## Findings
