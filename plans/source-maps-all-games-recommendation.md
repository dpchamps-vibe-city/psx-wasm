# Source-map feasibility: insufficient for required C-variable debugging

**Status:** earlier recommendation withdrawn as the solution to this task.
The user requires C-variable inspection in every game; a line-only baseline
with variable debugging deferred is not acceptable. The current recommendation
is [retain DWARF and repair value provenance](debug-builds-threaded-games-dwarf-repair.md).
The experiments and earlier proposal below are retained as research, not an
implementation selected for delivery.
**Related:** [threaded-debug investigation](debug-builds-threaded-games-investigation.md),
[original problem](debug-builds-threaded-games.md), D53/D56/D57 in
[design.md](../docs/design.md).

## Earlier proposal (not selected)

**Make standard Wasm source maps a baseline artifact for every game, including
threaded games and ordinary optimized builds. Do not make that depend on
repairing Asyncify's DWARF variable locations.**

Keep the existing full-DWARF `--debug` capability separately. Do not silently
change its meaning to “lines only,” remove the threaded-debug guard, or expose
known-invalid variable descriptions. This adds a useful capability without
claiming the original D53 problem is solved.

The important new finding is that **pinned Binaryen 133 supports source maps
through all three relevant stages: scoped Asyncify, `wasm-merge`, and final
`wasm-opt -O2`.** We do not need a Binaryen fork, a new transformation sidecar,
a replacement thread implementation, or an Emscripten build migration to
provide source-level navigation and breakpoints in every game.

The earlier proposal's user-visible contract (insufficient for the requirement):

| Capability | Ordinary builds, every game | Full-DWARF `--debug` |
| --- | --- | --- |
| Decompiled/generated C available in DevTools | Yes | Yes |
| Source-line breakpoints, location mapping, optimized stepping | Yes, subject to optimized-code limitations | Yes |
| C variable names, types, expressions and pointer inspection | **Not supplied by source maps** | Existing D53 capability/limitations |
| Threaded games | Supported by the proposed source-map path | Still blocked pending a correct DWARF solution |
| Guest window and execution pipeline | Existing ordinary-build layout and passes | Existing D53 layout and Rust merge |

“All games” should mean every game using the build pipeline, not a bof4
exception. Cover both decompiled C and lifted C, with container-qualified
source paths for overlays. A native Rust adapter is not a decompiled C
function: retain its Wasm name, but do not pretend it maps to game C.
Mapping runtime Rust sources would be a separate extension of source coverage.

## Why this differs from the earlier investigation

The earlier conclusion remains correct **for full D53 debugging**. It
established an explicit but false variable location after Asyncify. Passing
`llvm-dwarfdump --verify`, forcing DWARF 4, relocating PC endpoints, and
reattaching original debug sections cannot establish variable correctness.

This proposal interpreted “source maps” as source-line mapping alone. That
was a smaller requirement than the user intended. Its technical distinction
is still useful, but does not justify delivering it instead of C inspection:

- A source map describes **output code location → source location**. It does
  not assert where a variable's value resides.
- Binaryen copies source locations to transformed expressions and emits
  mappings in output order. Duplicates and reordering do not require an
  order-preserving one-input-address/one-output-address `CodeMap`.
- The project's `CodeMap` remains useful for its own guest rewrites. It need
  not become a general Asyncify value-provenance system to ship source maps.

D53 explicitly rejected source maps as a **replacement** for variables. This
recommendation is to add them as a **baseline**, not to erase that distinction.
An implementation should record that distinction in the design documentation.

## Research and new experimental evidence

### Primary-source findings

1. [Emscripten's debugging guide][emscripten] distinguishes DWARF variable
   debugging from source-map location mapping. It explicitly says source maps
   survive full post-link optimization and recommends `-gline-tables-only`
   when types and variables are unnecessary. Its driver generates maps from
   DWARF; we can use the same boundary without adopting the driver.
2. [Binaryen's maintainer guidance][binaryen-3462] recommends source maps over
   its partial DWARF support for transformations such as Asyncify.
3. The [pinned `wasm-merge` source][merge-source] reads per-input maps, including
   the syntax `guest.wasm guest -ism guest.wasm.map`, and writes a merged map.
   The installed binary's `--help` confirms these options. The project's
   ordinary merge currently supplies none of them.
4. [Binaryen's source-location helper][locations] explicitly permits copying
   one original expression's location to several replacement expressions.
   This is suitable for location mapping, not a claim about value identity.
5. [Wasm tool conventions][conventions] and [ECMA-426][sourcemaps] specify the
   external map, its `sourceMappingURL` custom section, and generated columns
   as **absolute byte offsets in the Wasm file**. This differs from our DWARF
   mapper's **code-section-payload-relative** addresses.

The earlier investigation already audited the later scope-range repair
[Binaryen #8964][range-repair]. That repair is relevant to eventual DWARF
work, not a prerequisite for the source-map pipeline demonstrated here.

### Tests run with the pinned toolchain

Used wasi-sdk 34's clang 23.1.0 and Binaryen 133. All exploratory programs and
outputs stayed in the supplied session scratchpad under `maps/`. Production
code, pins, game outputs and the threaded-debug guard were not changed.

#### 1. Real C lines through the complete transformation chain

Reused the investigation's `leaf`/`worker`/`root`/`branchy` fixture, including
its imported `psx.bios` suspension point and volatile stack slot:

```text
clang -O2 -gline-tables-only
  → linked Wasm DWARF line tables
  → source-map extraction and byte-preserving DWARF removal
  → scoped Asyncify + input/output source maps
  → wasm-merge with a small forwarding runtime + input/output source maps
  → wasm-opt -O2 + input/output source maps
  → final.wasm + final.wasm.map
```

A disposable Rust reader used `gimli` and `wasmparser` to extract the line
rows, convert coordinates, encode source-map VLQ segments and remove debug
custom sections. The forwarding runtime exported `psx.bios` and called the
host's `bios`, so the fixture exercised a real import-resolution merge.

| Stage | Mapped segments | Explicit unmapped segments | Distinct file/line/column locations |
| --- | ---: | ---: | ---: |
| Before Asyncify | 39 | 6 | 35 |
| After scoped Asyncify | 97 | 66 | 35 |
| After merge | 97 | 66 | 35 |
| After final `-O2` | 57 | 27 | 35 |

The final map was 423 bytes, without embedded source text. All 35 distinct
input source locations survived **in this fixture**, not a promise that
optimization preserves every source line in a game.

In a fresh headless Brave browser reporting Chromium **151.0.7922.137**:

- Streaming instantiation discovered `sourceMappingURL: final.wasm.map`.
- The browser's actual DevTools frontend loaded the map and fetched
  `fixture.c` over HTTP, without the C/C++ DWARF extension.
- Setting a breakpoint through DevTools' source-level breakpoint manager at
  C line 17 (`after = before + resumed + stack_value`) stopped after real
  suspension/rewind. `root(5)` returned **508**, with **two** suspension
  cycles and the Asyncify buffer pointer restored to **4104**.
- The source-line breakpoint resolved to two executable locations, producing
  four stops across the two worker calls. A source line need not mean one
  unique stop after optimization/transformation.
- `root` and `branchy`, each at inputs 0, 1, 5 and 100, matched the ordinary
  untransformed fixture: **eight comparisons**, with two or three real
  suspension cycles respectively. Every run restored the buffer pointer.

This is actual source-map frontend and breakpoint evidence. It is not a
DWARF-extension test, an exhaustive stepping test, or a game-browser test.

#### 2. Actual cached game modules through the remaining build stages

Ran separate mapped/unmapped controls from the same inputs and with the
project's actual Binaryen feature flags:

| Input | What was mapped | Stages exercised | Final mapped segments | Final map size |
| --- | --- | --- | ---: | ---: |
| `out/btn/guest-debug.wasm` | Real existing C line tables, 1,197 source paths; DWARF removed before Binaryen | `wasm-merge` → `wasm-opt -O2` | 110,032 | 707,107 bytes |
| `out/bof4/guest-linked.wasm` | Synthetic unique location for each of 3,987,213 input opcodes | Scoped Asyncify using existing `asyncify-only.txt` → `wasm-merge` → `wasm-opt -O2` | 2,782,965 | 14,423,864 bytes |

For **both**, every non-custom Wasm section in the mapped final module was
byte-identical to the corresponding map-free control. Thus carrying maps
through those stages did not alter executable sections in these tests.
The bof4 final map's mapped offsets all landed on output opcode starts.

Important qualifications:

- bof4's dense synthetic map tests the real module's transformation channel,
  **not actual C source association or an expected production map size**.
- btn started with its cached **debug-window** guest, not a freshly rebuilt
  ordinary-layout game. The comparison controls used that same input.
- Neither experiment rebuilt the complete game from C or ran a game replay.
- Some real DWARF line rows point at function entries/local declarations,
  rather than executable opcodes. Breakpoint resolution must account for
  that; “every line row is an opcode” is not a valid blanket invariant.

Separately, `-gline-tables-only` versus no debug info produced identical
non-custom sections for one actual bof4 lifted object (`exe/801395A0.c`)
and one btn decompiled object (`ovl_15_func_80131498.i`). The standalone
fixture also matched across no debug info, line tables, and full `-g`.
These are sampling controls, not a compiler-wide guarantee.

## Earlier implementation proposal (not selected)

### 1. Collect line information independently of the full-debug layout

Compile normal source-mapped guest objects with `-gline-tables-only` and the
existing relative compilation directories, `source/game` and `source/lift`.
Keep `-O2`, UB-containment flags, call policy, safepoints and Asyncify scope
unchanged. Preserve full `-g` for the existing full-debug mode.

Do **not** turn on `opts.debug` merely to obtain line tables: today it also
selects the 2-GiB debug window and another final merge/optimization path.
Separate collection of debug metadata from those choices. In particular,
`GameSpec.debug_info` can request guest-link metadata without requiring the
D53 memory layout.

Initially reuse the existing DWARF relocation/link code with line-table-only
inputs, then convert the linked guest's relocated line rows into a map. This
avoids writing a second object-relocation engine. Extract and remove DWARF
**before** Binaryen. Do not generate the initial map from already-damaged
post-Asyncify DWARF.

Object caches must include the new flags: ordinary lifted objects and the
`calls/` variants currently do not share the debug directory's flag stamp.
Reusing old objects would otherwise silently produce partial/empty maps.

### 2. Carry the map through the existing ordinary-build pipeline

The relevant invocation shape, with today's feature flags and scope intact:

```sh
wasm-opt guest-linked.wasm -g --asyncify \
  --pass-arg=asyncify-imports@psx.bios \
  --pass-arg=asyncify-onlylist@@asyncify-only.txt \
  -ism guest-linked.wasm.map -osm guest.wasm.map \
  -osu guest.wasm.map -o guest.wasm

wasm-merge guest.wasm guest -ism guest.wasm.map runtime.wasm psx \
  -g -osm merged.wasm.map -o merged.wasm

wasm-opt merged.wasm -O2 -g \
  -ism merged.wasm.map -osm game.wasm.map \
  -osu game.wasm.map -o game.wasm
```

Nonthreaded games skip only the Asyncify step. Function names remain enabled
with Binaryen's existing `-g`; that flag alone is **not** source-map generation.
For existing supported full-DWARF builds, derive the additional source map
from final relocated DWARF after the Rust merge instead of sending that build
through the ordinary pipeline.

Implementation belongs primarily in `crates/psxw-link/src/dwarf.rs` (or a
small source-map module) and `crates/psxw-build/src/game.rs`. No new general
provenance abstraction is needed.

### 3. Bundle the maps and the exact named sources

Update `crates/psxw-build/src/web.rs`: it currently discovers sources only
through final `.debug_*` sections. A correctly stripped source-map build
would therefore copy **no C at all** without this change.

- Generate maps as normal build artifacts and include them by default in
  every game web bundle. Keep each map paired with its exact Wasm output.
- Ship `game.wasm.map` and the map for `guest.wasm` used by `?dev`, each with
  its own correct URL, including nonthreaded guests. Never reuse the final
  module's map for the guest. Preserve per-build-variant artifact names and
  reconcile URLs when bundling renames `game-<variant>.wasm` to `game.wasm`.
- Resolve relative source URLs against the **map's URL**. Keeping maps beside
  the Wasm preserves the existing `source/game/…` and `source/lift/…` scheme.
- Copy the exact pinned/generated sources, not a developer's current decomp
  checkout. Keep these game-derived bundles local, as current policy requires.
- Prefer separate source files initially; do not depend on Binaryen retaining
  `sourcesContent`. If embedding is later wanted, populate it after the last
  transform.
- Serve `.map` as JSON (`crates/psxw-cli/src/serve.rs`); retain existing
  cross-origin headers and streaming Wasm instantiation.

Maps describe **final absolute file offsets**. Recompute the code-section
base after any operation that moves it. Strip obsolete `sourceMappingURL`
sections, emit exactly one correct URL, and append final metadata rather
than inserting it ahead of code. Preserve explicit unmapped segments,
sequence ends, and gaps between source-backed functions so helpers/runtime
code do not inherit the preceding C line. Do not fill gaps by guessing.

## Acceptance: three focused gates

1. **Transformation integration fixture.** Use real C lines through guest
   rewrites, scoped Asyncify, merge and final optimization. Verify map
   encoding/coordinates and source resolution; hit a source breakpoint
   before/after actual suspension, including a branch/loop case. Compare
   executable sections against a map-free control. Keep the fixture small.
2. **Real game bundle acceptance.** Freshly build btn and bof4, with actual
   C maps. In the normal Worker and `?dev` paths, check source retrieval,
   line breakpoints and stepping around ChangeThread; cover instrumented
   and uninstrumented code, decompiled/lifted C, overlays, and a script
   mod on a supported moddable bundle. Check Chromium and the supported
   Firefox path. Quantify surviving source coverage and map/build costs.
   Do not label a game “mapped” just because it has a nonempty map.
3. **Execution regression gate.** Run bof4 `title-to-field` and existing btn
   replay checks against map-free builds, including reached-state, timing,
   RAM/VRAM/audio results. Preserve D57's instrumentation set. Existing
   full-DWARF btn debugging must not regress; check its interaction with
   the additional map when the C/C++ extension is installed.

The experiments establish feasibility of the chosen metadata channel, not
completion of these project-level gates. Actual bof4 C mapping, full builds,
replays, Worker/dev-mode acceptance and Firefox remain untested here.

## Mandatory variables rule this out as the solution

The user confirmed that variables must work in every game. This is **not the
recommended first delivery**. The [DWARF follow-up](debug-builds-threaded-games-dwarf-repair.md)
now reports a producer-correctness experiment addressing the questions below:

1. Isolate the first wrong value location in Binaryen roundtrip, flattening,
   and Asyncify, including the project's guest rewrites as a baseline.
2. Assess a focused repair of the existing DWARF updater, including location
   validity intervals and explicit unavailability when a value cannot be
   represented. Measure useful remaining coverage on real lifted code.
3. Choose an in-Binaryen repair versus exported transformation metadata only
   after that experiment establishes what data is missing. Do not begin by
   designing a general sidecar or replacing Asyncify.

Neither a Binaryen-only upgrade, separate DWARF files, nor new source-map
fields supplies the missing value provenance by itself. If full variables
are mandatory in the first release, the current evidence does **not** justify
claiming that release is ready or that a small flag-only solution exists.

**Current conclusion:** source-line mapping is feasible, but this proposal
does not meet the required C-variable inspection. Prioritize the DWARF repair
in the follow-up, not this source-map-only baseline.

## Reproduction notes

Use the C fixture in the original investigation with:

```sh
clang --no-default-config --target=wasm32-unknown-unknown \
  -nostdlib -O2 -gline-tables-only -fdebug-compilation-dir=. \
  -fno-builtin -Wl,--no-entry -Wl,--export=root -Wl,--export=worker \
  -Wl,--export=leaf -Wl,--export=branchy fixture.c -o fixture-lines.wasm
```

Extract relocated line-table rows to a v3 source map: add the final
code-section payload's file offset to DWARF PCs; convert source line/column
coordinates to zero-based; emit unmapped boundaries; strip debug sections
without re-encoding code. For the standalone fixture use the Asyncify
onlylist `worker,root,branchy`. Merge with a Wasm module exporting `bios`
that forwards its `i32` argument/result to `host.bios`, then run the final
optimization with input/output maps as above.

Use the original investigation's descriptor at 4096, stack start 4104,
end 30000 and unwind/rewind host protocol. Serve the Wasm, map and C from
the same directory. Set a C-line-17 breakpoint through DevTools. The measured
final mapped offsets on that line were 451, 456, 460 and 461; the frontend's
line breakpoint selected 456 and 460. Re-derive offsets if inputs change.

The session scratchpad's `maps/` contains `probe.rs`, `fixture.c`,
`runtime.wat`, `game-pipelines.sh`, `analyze.py`, `browser.mjs`, JSON results
and logs. Those are disposable evidence, not production implementations.
The method and conclusions above do not require retaining the scratchpad.

[emscripten]: https://emscripten.org/docs/porting/Debugging.html
[binaryen-3462]: https://github.com/WebAssembly/binaryen/issues/3462
[merge-source]: https://github.com/WebAssembly/binaryen/blob/fba5005a132d85d578be5bd010e0487ed8e2ce3e/src/tools/wasm-merge.cpp
[locations]: https://github.com/WebAssembly/binaryen/blob/fba5005a132d85d578be5bd010e0487ed8e2ce3e/src/ir/debuginfo.h
[conventions]: https://github.com/WebAssembly/tool-conventions/blob/main/Debugging.md
[sourcemaps]: https://tc39.es/ecma426/
[range-repair]: https://github.com/WebAssembly/binaryen/pull/8964
