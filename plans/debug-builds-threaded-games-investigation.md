# Debug builds of threaded games: investigation

**Status:** findings and open feasibility questions; no implementation selected or enabled.
**Problem:** [debug-builds-threaded-games.md](debug-builds-threaded-games.md).
**Design refs:** D53, D56, D57 in [docs/design.md](../docs/design.md).

**Latest follow-up:** [Retain DWARF and repair value provenance](debug-builds-threaded-games-dwarf-repair.md).
C-variable inspection is mandatory. New experiments isolate the demonstrated
failure to Flatten and recover values in the browser DWARF extension with an
incomplete producer-side patch. That is the recommended repair direction,
not yet an enabled all-games solution. The [source-map-only proposal](source-maps-all-games-recommendation.md)
is withdrawn as the solution. Findings and limits below describe the original
investigation; the follow-up records the subsequent evidence.

## Conclusion

**This is not an inherent incompatibility between Asyncify and DWARF, but
there is no demonstrated flag-only fix that meets the existing contract.**

The problem statement's “Asyncify drops the DWARF” is too broad:

1. For the project's `wasm32-unknown-unknown` target, the pinned clang's
   plain `-g` emits **DWARF 4**. Binaryen 133 retains and relocates DWARF 4
   through Asyncify; the small test modules pass `llvm-dwarfdump --verify`.
2. Nevertheless, the transformed DWARF can describe **the wrong runtime
   location of a variable at a valid source breakpoint**. This was reproduced
   using V8's inspector, not inferred just from a warning or missing section.
   This is the demonstrated correctness blocker in the current format.
3. Explicit `-gdwarf-5` inputs expose a separate limitation: Binaryen 133's
   DWARF writer aborts, even without Asyncify. **DWARF 5 rejection does not
   explain the current plain-`-g` path**, and forcing DWARF 4 is not a fix.
4. The current Rust DWARF mapper cannot repair the observed value movement
   merely by relocating addresses. A richer mapping is one possible repair
   boundary, not evidence that a new sidecar interface is necessary.

The evidence establishes a metadata defect, not the smallest repair or an
end-to-end solution for bof4. Removing the guard is not a demonstrated fix.
The diagnostic should not be taken literally, but neither it nor any other
production code was changed in this investigation.

## What was inspected and run

- Project build, Asyncify scope, DWARF conversion, and Rust merge paths:
  `crates/psxw-build/src/game.rs`,
  `crates/psxw-link/src/{asyncify,dwarf,merge}.rs`.
- Actual tools from `inputs.lock`: wasi-sdk 34, clang 23.1.0-wasi-sdk,
  Binaryen 133, and the SDK's `llvm-dwarfdump`/`llvm-objdump`.
- Binaryen `version_133` source:
  `fba5005a132d85d578be5bd010e0487ed8e2ce3e` (2026-09-21).
- A later upstream source snapshot, **not an upgraded project toolchain**:
  `c3848522eb570261aa450acce6f81b7b065c5c1c` (2026-10-29).
- Internet searches followed by reading upstream issues, a merged repair PR,
  Emscripten/Chrome guidance, and the wasm DWARF convention (references below).
- A standalone C fixture with an ordinary leaf, a worker with locals and a
  volatile stack slot, a root calling the worker twice, and a branch/loop
  function. No game-derived fixture was added to the repository.
- A small Rust executable using the project's actual `merge::merge` and
  `dwarf::Dwarf`, plus raw wasm breakpoints and scope inspection through
  Node 24.1.0 / V8 13.6.233.10's inspector.

Disposable code, source checkout, logs, wasm, and JSON results are under the
session scratchpad's `thread-debug/` and `binaryen-133/`. The scratch Rust
build directory was removed after use; its source and executable remain.
The observations and reproduction recipe below do not depend on retaining
that temporary directory.

### Explicit limits

- No full bof4 debug build, `title-to-field` replay comparison, or new web
  bundle was made. The guard remains in place.
- No actual Chrome C/C++ DWARF extension UI acceptance was performed. The
  debugger evidence below is the raw wasm state at the DWARF line address,
  interpreted according to the documented wasm DWARF convention.
- The newer upstream snapshot was audited as source, not built and tested.
- No new thread implementation, runtime behavior, pins, or design decisions
  were introduced. Existing unrelated working-tree edits were left alone.

## 1. What Binaryen actually does

### The current plain-`-g` path emits DWARF 4

A final control compiled the fixture with the pinned clang, the project's
`--no-default-config --target=wasm32-unknown-unknown`, and plain `-g`.
`llvm-dwarfdump` reports a version-4 compilation unit. Scoped Asyncify on
that module succeeds and its DWARF passes verification. The first units in
the existing cached `out/btn/guest-debug.wasm` also report version 4.

The initial draft incorrectly called DWARF 5 the default. This correction
matters to the options: `-gdwarf-4` is not a format workaround for our current
build, and a new interface to bypass Binaryen's DWARF 5 writer has not been
shown necessary. The semantic failure in §2 was measured with explicit
`-gdwarf-4`, the same DWARF version as the plain-`-g` control.

### Explicit DWARF 5 fails independently of Asyncify

Compile the fixture at `-O0` or `-O2`, with `-gdwarf-5`, then run either a
plain `wasm-opt -g` roundtrip or the project's Asyncify invocation. All four
cases abort in the pinned Binaryen writer. Observed messages include:

```
warning: unsupported DWARF version (5)
compile unit size was incorrect (this may be an unsupported version of DWARF)
```

The optimized input also exposes an `unsupported form` failure. Stripping
DWARF before the pass succeeds, but of course gives no variable debugging.

The cause is in Binaryen's `src/wasm/wasm-debug.cpp`: the DWARF support reads
through its vendored LLVM DWARFContext, converts to its DWARFYAML
representation, updates it, and emits it again. It explicitly warns for
versions above 4. This is a format-support limitation, separate from whether
Asyncify can relocate debug information accurately. It is relevant if the
project adopts DWARF 5, not the explanation for its present debug format.

### DWARF 4 survives, but survival is not correctness

The experiment matrix, using the same source and pinned tools:

| C optimization | DWARF | Binaryen operation | Result |
| --- | --- | --- | --- |
| `-O2`, plain clang `-g` | 4 (default) | scoped Asyncify with `-g`, no Binaryen `-O` | succeeds; DWARF retained; verifier passes |
| `-O0`, `-O2` | 4 (explicit) | roundtrip with `-g` | succeeds; DWARF retained; verifier passes |
| `-O0`, `-O2` | 4 | scoped Asyncify with `-g`, no Binaryen `-O` | succeeds; DWARF retained; verifier passes |
| `-O0`, `-O2` | 4 | explicit `--flatten -g` | succeeds and warns about DWARF incompatibility |
| `-O0`, `-O2` | 5 | roundtrip or scoped Asyncify with `-g` | aborts |
| `-O0`, `-O2` | 4 or 5 | `--strip-dwarf`, then scoped Asyncify | succeeds; no DWARF |

The distinction between **clang `-O2`** and **Binaryen optimization** matters.
The project already uses optimized C, but its `--asyncify` invocation does
not request Binaryen `-O`; this investigation did not add it as a workaround.

### Why `--flatten` is relevant even without Binaryen optimization

Asyncify unconditionally runs `flatten` and `dce` on the selected functions,
then rewrites their control flow and adds local saving/restoring. Optional
optimization passes are additional work, not what causes flattening here.

Binaryen 133's `Flatten.cpp` explicitly says:

> Flattening splits the original locals into a great many other ones, losing
> track of the originals that DWARF refers to.
> FIXME DWARF updating does not handle local changes yet.

Its `invalidatesDWARF()` returns true. Asyncify itself does not advertise that
invalidation. Its nested `FilteredPassRunner`s are constructed without the
outer debug options, so the direct-flatten warning is absent from the tested
Asyncify invocation. **Silence from `wasm-opt` is not a safety check.**

The writer also has a DWARF-specific local-emission path
(`BinaryInstWriter::mapLocalsAndEmitHeader` in `wasm-stack.cpp`). It avoids
regrouping the locals when DWARF is present. That protects existing local
indices in supported cases; it does not relocate an old operand-stack value
into a new local. Binaryen's `updateLoc` explicitly leaves the location
expression itself unchanged.

## 2. A verifier-clean, demonstrably wrong variable location

The important part of the fixture is:

```c
__attribute__((noinline)) int worker(int input) {
    int before = input + 11;
    volatile int stack_value = before * 2;
    int first = leaf(before);
    int resumed = bios(first);
    int after = before + resumed + stack_value;
    observed = after;
    return after;
}
```

Here `bios` is an imported `psx.bios` function. The host initiates an unwind
on its first invocation, then stops the rewind on re-entry and returns the
argument plus 100. `root(5)` calls `worker(5)` and `worker(6)`.

### Before Asyncify

At the source row for the `after` calculation, clang's DWARF describes
`resumed` as:

```
[0x51, 0x54): DW_OP_WASM_location 0x2 0x1, DW_OP_stack_value
```

`0x2` means the wasm operand stack; index 1 is counted from its bottom.
The code section payload starts at file offset `0x64`, so a raw breakpoint
at `0xB5` is precisely the row at code offset `0x51`.

V8 shows operand stack `[0, 198]` on the first worker call and `[0, 211]`
on the second. The description is correct at that breakpoint.

### After scoped Asyncify

The rewritten line table places the same source row at code offset
`0x13D`. The location list has been relocated to:

```
[0x13D, 0x146): DW_OP_WASM_location 0x2 0x1, DW_OP_stack_value
```

The code section now starts at file offset `0xF2`. A raw breakpoint at
`0x22F` therefore lands exactly on this source row and inside this location
list entry. Observed state on the first call:

| State | Value |
| --- | --- |
| operand stack | **empty** |
| wasm local 0 | 16 (`before`) |
| wasm local 1 | 65520 (C frame base) |
| wasm local 11 | 98 (`first`) |
| wasm local 12 | **198 (`resumed`)** |

On the second call, the corresponding locals hold 17, 111, and 211. In both
cases there is **no operand-stack slot 1**. Asyncify/flattening moved the
value into a temporary local, but the DWARF still asks for its old slot.

This is not merely the ordinary loss of an optimized-out variable: there is
an explicit, nonempty location expression claiming a location that is false.
A consumer may show an error or unavailability, but the producer has not
correctly marked it unavailable or described where the value went.

The module passes `llvm-dwarfdump --verify`. That verifier checks structural
DWARF properties; it does not prove the description matches wasm execution.

### Execution and merge controls

- Plain `root(5)` returns **508**.
- Asyncified `root(5)` returns **508**, with **two actual suspend/resume
  cycles**, returning the Asyncify buffer pointer to its initial position.
- The project's Rust merge and DWARF relocation also produce a validating,
  verifier-clean module returning **508** after two cycles. The incorrect
  variable description survives that merge; merge is not its cause or cure.
- Across C `-O0`/`-O2`, `root`/`branchy`, and inputs 0, 1, 5, 100, **16
  equivalent cases / 56 executions** matched between plain, Asyncified with
  DWARF, Asyncified without DWARF, and (for C `-O2`) Rust-merged variants.
  `root` suspends twice; `branchy` suspends three times through its branch
  and loop work.

These controls are fixture evidence, not a substitute for the game replay.

Operand-stack DWARF is not an exotic compiler output. Reading the existing
cached `out/btn/guest-debug.wasm` with gimli found **8,452 occurrences of
`DW_OP_WASM_location` operand-stack descriptions**, alongside 14,953 local
descriptions. These count operations in expressions, not distinct variables
or a prediction of how many bof4 variables fail.

## 3. Can the existing `CodeMap` carry the transformation?

Not as it stands.

`FuncMap` records input/output instruction offsets. `CodeMap::map` returns
one output address for an input address, with a floor lookup. Line conversion
walks the original rows and clamps output row offsets to be nondecreasing;
range and location conversion remaps only the two endpoints. Expressions
are converted without a wasm value-location transformation.

That works for the project's existing order-preserving guest rewrites and
call-index changes in its merge. Asyncify needs more:

- one original expression can have an original operation and replacement
  reads/copies at several output addresses;
- output code can appear in a different provenance order;
- one original range may need several output ranges, with synthetic-code
  gaps;
- old operand-stack locations may become temporary locals; original locals,
  frame bases, globals, and synthetic execution states must be distinguished;
- an unmappable description must become unavailable, not acquire a guessed
  nearby address or value.

### Synthetic source-map experiment

With the **unmodified pinned tool**, an input source map assigned a unique
synthetic source location to every input opcode. DWARF was stripped outside
Binaryen, and Asyncify consumed/emitted this map through its normal
`--input-source-map` / `--output-source-map` support.

For the optimized fixture:

- 96 input opcode starts;
- 828 output opcode starts;
- 149 mapped output rows representing 94 distinct input opcode offsets;
- 66 explicit unmapped markers;
- 11 decreases in original offset when walking mapped rows in output order.

This demonstrates a usable **provenance channel**, including duplicates and
reordering, not a faithful DWARF transform by itself. Source-map locations
can be copied to replacement expressions; they are not assertions that an
old stack layout still holds there. Function ends, delimiters, locations
with no surviving row, and the intervals between rows also need handling.

Using source maps internally to help rebuild DWARF is distinct from shipping
source maps instead of DWARF. The latter still fails the variable-inspection
requirement. The former remains a candidate that needs value provenance,
not just line/address conversion.

## 4. What upstream research changes

### “Asyncify strips DWARF” is not upstream's contract

In [Binaryen #3462][upstream-3462], a maintainer explicitly states that
`wasm-opt --asyncify` should not strip DWARF, while warning that its partial
DWARF support gets worse with transformations such as Asyncify. This matches
the pinned-tool observations, not the current build diagnostic.

### DWARF 5 has been a known separate problem

[Emscripten #14241][emscripten-14241] documents DWARF 5 failing in Binaryen's
writer even on an ordinary roundtrip, and the limitations of the older
DWARF 4 path. The actual 133 failures were reproduced here rather than
assuming that an old issue describes today's binary.

### A relevant newer repair exists—but does not solve value locations

[Binaryen PR #8964][upstream-8964] was merged on 2026-10-01 as
`06f14fc522372ed89e2b350ca0948eb34006d981`, after version 133. It addresses
lost/reversed endpoints, tombstone handling, range normalization and parent/
child/sibling scope topology. It fixes the class of impossible ranges
reported in [#6406][upstream-6406].

The later source snapshot includes that repair, but still:

- warns on DWARF versions greater than 4;
- marks `Flatten` as invalidating DWARF;
- leaves the `.debug_loc` location expression unchanged in `updateLoc`.

Therefore “upgrade to current main” is not evidence of a complete fix. The
range repair is relevant work to reuse or learn from; it is not an
operand-stack-to-local mapper.

Additional controls used several upstream DWARF fixtures. Some already fail
verification on input and cannot honestly be counted as Asyncify regressions.
`fannkuch3_manyopts_dwarf` verifies on input and after unoptimized Asyncify,
but fails after `-O --asyncify -g`, reinforcing that adding Binaryen
optimization expands the problem. The project does not use that invocation
for its Asyncify step.

### External DWARF and debug fission do not evade relocation

[Emscripten's debugging guide][emscripten-debug] recommends avoiding Binaryen
when possible and warns that it degrades DWARF. [Chrome's scaling
article][chrome-debug] describes separate/split debug data and
`ERROR_ON_WASM_CHANGES_AFTER_LINK`.

Moving debug data to a side file does not make its addresses or wasm variable
locations independent of the executable. The [wasm DWARF convention][wasm-dwarf]
still ties PCs to code-section offsets and values to actual wasm
locals/globals/operand-stack entries. A side file needs the same repair.

## 5. Remaining feasibility questions

### Where does the first incorrect description appear?

The existing results do not yet isolate the earliest failing stage:

| Stage | Evidence obtained | Still missing |
| --- | --- | --- |
| Plain clang, DWARF 4 | `resumed` is correct at the inspected source row | Broader variable/type coverage |
| Binaryen roundtrip | Structural verification passes | Runtime variable inspection |
| `--flatten` alone | Structural verification passes; incompatibility warning | Runtime variable inspection |
| Scoped Asyncify | Verifier-clean but false location at the source row | Isolation of the responsible internal transformation |
| Asyncify → Rust merge | Execution matches; false description remains | Browser inspection of the final artifact |

Flattening is a strong source-level lead, not an experimentally isolated
first failure. Comparing the same variable at equivalent source rows after
each stage would narrow the repair. Include the project's guest rewrites
as a baseline too: the standalone fixture did not pass through those before
Asyncify. An upstream reproducer should distinguish a roundtrip defect from
a flattening or Asyncify-specific defect.

### How small can a correct repair be?

Since the current format is already DWARF 4, first investigate whether a
focused repair of Binaryen's existing updater can track the lost locations.
That is a question about patch scope, not a commitment to patch Binaryen.
Determine whether the needed value identities survive in its IR, which
transformations lose them, and whether existing binary-position/local-index
tracking is sufficient or needs additional annotations.

Only then compare an in-Binaryen repair with exporting transformation data
to Rust. The source-map experiment proves neither sufficient value tracking
nor the necessity of a new interface. The newer upstream scope repair is
relevant to ranges, but is not evidence that either boundary solves values.

### How representative is the observed failure?

Measure variable-location coverage in actual lifted code, partitioned by the
D57 instrumentation set and location kind: locals, operand stack, globals,
frame-relative memory, and constants. Uninstrumented functions also pass
through Binaryen's reader/writer and need controls. The cached btn count in
§2 establishes that operand-stack descriptions occur often; it says neither
how many bof4 locations fail nor which remain trustworthy.

### What constitutes acceptable variable availability?

D53 already accepts ordinary `-O2` optimized-out variables. The question is
whether additional, **correctly reported unavailability** after Asyncify is
acceptable, and how much useful inspection must remain. That differs from
a nonempty location expression claiming the wrong value or a nonexistent
slot. The current experiment demonstrates the latter.

Conservative removal of invalid descriptions might be a smaller route, but
its safety and remaining coverage are unmeasured. Deleting operand-stack
descriptions alone would not validate the remaining scopes, ranges, locals
or frame bases. Do not assume either that every source variable must always
be recoverable or that losing most variable inspection meets D53.

### What does the browser actually show?

The V8 experiment proves a contradiction between DWARF and runtime state.
It does not measure Chrome's C/C++ extension's UI, stepping behavior, error
handling or available-variable coverage. Those checks, and the full bof4
replay, remain necessary before claiming user-visible feasibility.

## 6. Options—not a selected implementation

| Approach | Assessment |
| --- | --- |
| Remove the guard; keep `-g` | The current format can pass through Asyncify, but the fixture demonstrates incorrect variable metadata. No bof4 acceptance has been run. |
| Force `-gdwarf-4` | Already the observed default for this target; does not repair the demonstrated defect. |
| Save and reattach the original debug sections | Leaves stale code offsets, ranges and value locations after the pass. |
| Ship source maps instead | Helps stepping; does not supply D53's variable inspection. |
| Upgrade Binaryen only | Relevant scope repairs exist; audited newer source still leaves location expressions unchanged. Not a demonstrated fix for the observed value error. |
| Repair Binaryen's existing DWARF 4 path | Candidate worth investigating for the current format; extent of required value tracking and scope repair is unknown. |
| Export transformation metadata and repair DWARF in Rust | Another candidate, requiring more than the current `CodeMap`; no sufficient sidecar or value-provenance scheme has been demonstrated. |
| Conservatively mark invalid descriptions unavailable | Possible availability tradeoff, subject to the correctness and coverage questions above. |
| Implement an independent Rust Asyncify-like pass | Could own precise metadata, but duplicates substantial control-flow, liveness and rewind machinery. Not justified by the findings so far. |
| Replace guest threading with JSPI/stack switching or another runtime model | Reopens D57 and cross-host behavior/compatibility. The defect is localized enough to investigate metadata first. |

No implementation boundary is selected. In particular, the original draft's
preference for a new sidecar rested partly on its incorrect DWARF-default
assumption and is withdrawn. The next research step is stage isolation and
assessment of the smallest correct repair, not building a new interface.

If richer transformation metadata proves necessary, it would need to cover
function/expression/delimiter positions, emitted indices, value movements,
validity ranges, removed code and synthetic regions. Binaryen has some of
these building blocks internally, not a demonstrated complete mapping.
Keep any such representation separate from `CodeMap` unless its semantics
can actually be expressed by that existing order-preserving abstraction.

Any eventual toolchain change must remain pinned. Metadata handling must not
silently change pass options, scope or code generation; Binaryen's existing
DWARF-preservation mode can alter local emission, so “only metadata changed”
would itself need verification.

## 7. Acceptance evidence still needed

These are criteria for evaluating a future candidate, not implementation
tasks authorized by this review:

1. Inspect `before`, `resumed` and `stack_value` at equivalent source rows
   before/after real suspension, including more than one suspension site.
   Check correctness and explicit unavailability separately.
2. Cover branches, loops, recursion, direct and indirect calls, mixed wasm
   value types, inlined/lexical scopes, frame bases, optimized-out values,
   and instrumented/uninstrumented functions. Quantify remaining coverage.
3. Distinguish normal execution from synthetic rewind/unwind code. Inspecting
   the active wasm stack is not the same as recovering another guest
   thread's saved Asyncify buffer; do not promise the latter accidentally.
4. Verify final DWARF and inspect runtime values through the browser DWARF
   frontend. Neither structural verification nor raw V8 scopes substitutes
   for the other checks.
5. Exercise guest rewriting, Asyncify, Rust merge and served source paths,
   for decompiled and lifted C and a script mod against a debug base.
6. Compare D57's actual instrumentation set and bof4 `title-to-field` timing,
   reached-state checks, RAM/VRAM/audio results against the ordinary build;
   retain nonthreaded btn coverage. Record build time/size/memory costs too.

“Same code as release apart from `-g`” also needs a precise acceptance
statement: debug already relocates the guest window, uses a different final
merge and omits release's final `wasm-opt -O2`. Identical compilation and
instrumentation policies plus equivalent guest-visible execution are
meaningful; byte-identical final modules are not the existing behavior.

## Reproduction recipe

Save the following standalone fixture as `fixture.c`. It was compiled with
explicit `-gdwarf-4` to control the format; plain `-g` was checked separately
and also emits DWARF 4 for the project's target. The numeric offsets in §2
refer to this source and the pinned tools:

```c
typedef unsigned int u32;
__attribute__((import_module("psx"), import_name("bios")))
extern int bios(int);
volatile int observed;

__attribute__((noinline)) int leaf(int a) {
    int local = a * 3 + 7;
    observed = local;
    return local ^ 0x55;
}

__attribute__((noinline)) int worker(int input) {
    int before = input + 11;
    volatile int stack_value = before * 2;
    int first = leaf(before);
    int resumed = bios(first);
    int after = before + resumed + stack_value;
    observed = after;
    return after;
}

__attribute__((noinline)) int root(int input) {
    int left = worker(input);
    int right = worker(input + 1);
    return left + right;
}

__attribute__((noinline)) int branchy(int input) {
    int saved = input * 13;
    int result;
    if (input & 1) {
        result = bios(input + 2);
        observed = result + saved;
    } else {
        result = bios(input - 2);
        observed = result - saved;
    }
    for (int i = 0; i < 2; i++) {
        result += bios(result + i);
    }
    return result + saved;
}
```

Compile with the pinned SDK's clang, then run the pinned `wasm-opt`:

```sh
clang --no-default-config --target=wasm32-unknown-unknown \
  -nostdlib -O2 -gdwarf-4 -fno-builtin -Wl,--no-entry \
  -Wl,--export=root -Wl,--export=worker -Wl,--export=leaf \
  -Wl,--export=branchy fixture.c -o fixture.wasm
wasm-opt fixture.wasm -g --asyncify \
  --pass-arg=asyncify-imports@psx.bios \
  --pass-arg=asyncify-onlylist@worker,root,branchy \
  -o fixture-asyncify.wasm
llvm-dwarfdump --verify fixture-asyncify.wasm
llvm-dwarfdump --debug-info --debug-line --debug-loc fixture-asyncify.wasm
llvm-objdump -d fixture-asyncify.wasm
```

Host protocol for real suspension:

- Reserve an Asyncify descriptor at memory 4096; initialize its stack pointer
  to 4104 and its end to 30000, as little-endian `u32`s.
- On a normal `bios(x)`, call `asyncify_start_unwind(4096)` and return a dummy
  result. On return to the host, call `asyncify_stop_unwind()`.
- Call `asyncify_start_rewind(4096)` and re-enter the same root with the same
  arguments. On reaching `bios(x)` during rewind, call
  `asyncify_stop_rewind()` and return `x + 100`.
- Repeat if another normal call starts another unwind.
- With the exact pinned output, set a V8 `Debugger.setBreakpoint` at
  `{scriptId, lineNumber: 0, columnNumber: 0x22F}`. Read its
  `wasm-expression-stack` and `local` scopes using `Runtime.getProperties`.
  This is the first line-17 row; it is also inside `resumed`'s mapped range.
- For the untransformed control, return `x + 100` directly and break at
  `0xB5`; operand-stack slot 1 exists and has the expected value.

Offsets are tied to this fixture, export order and pinned toolchain. Derive
them again from the line table and code-section payload if changing any of
those inputs. Runtime values, source-row association and real suspension
are the assertions, not the literal offsets.

## References

[upstream-3462]: https://github.com/WebAssembly/binaryen/issues/3462#issuecomment-2560760060
[emscripten-14241]: https://github.com/emscripten-core/emscripten/issues/14241
[upstream-8964]: https://github.com/WebAssembly/binaryen/pull/8964
[upstream-6406]: https://github.com/WebAssembly/binaryen/issues/6406
[emscripten-debug]: https://emscripten.org/docs/porting/Debugging.html
[chrome-debug]: https://developer.chrome.com/blog/faster-wasm-debugging/
[wasm-dwarf]: https://yurydelendik.github.io/webassembly-dwarf/

Pinned source anchors:

- [Asyncify: flattening, scoped passes, control flow, saved locals](https://github.com/WebAssembly/binaryen/blob/fba5005a132d85d578be5bd010e0487ed8e2ce3e/src/passes/Asyncify.cpp)
- [Flatten: invalidation and temporary locals](https://github.com/WebAssembly/binaryen/blob/fba5005a132d85d578be5bd010e0487ed8e2ce3e/src/passes/Flatten.cpp)
- [DWARF address/range/location updater](https://github.com/WebAssembly/binaryen/blob/fba5005a132d85d578be5bd010e0487ed8e2ce3e/src/wasm/wasm-debug.cpp)
- [DWARF-sensitive local emission](https://github.com/WebAssembly/binaryen/blob/fba5005a132d85d578be5bd010e0487ed8e2ce3e/src/wasm/wasm-stack.cpp)
- [Source-location copying is not exclusive provenance](https://github.com/WebAssembly/binaryen/blob/fba5005a132d85d578be5bd010e0487ed8e2ce3e/src/ir/debuginfo.h)
- [Later audited DWARF updater](https://github.com/WebAssembly/binaryen/blob/c3848522eb570261aa450acce6f81b7b065c5c1c/src/wasm/wasm-debug.cpp)
