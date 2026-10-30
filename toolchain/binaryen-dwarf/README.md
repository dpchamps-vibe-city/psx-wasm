# C debugging through Asyncify

This directory contains a patched build of Binaryen's `wasm-opt` used to keep
C source breakpoints and variable inspection working in games that switch
threads.

Binaryen transforms WebAssembly code. Its **Asyncify** transformation lets a
function suspend and later resume; psx-wasm uses this to switch the emulated
PlayStation's threads. **DWARF** is the debug information that tells a debugger
which C line is running and where to find each variable. Ordinary source maps
provide source locations, but not the variable and type information needed here.

> **Validation status:** the integration test is recorded as passing with the
> current patch. The browser and full-game results below are from an older
> patch and still need to be rerun after the 2026-10-30 changes. See
> [recorded results](#recorded-results) and the
> [open tasks](../../plans/open-items.md#debug-builds-of-threaded-games-d53-d57).

## What goes wrong without the patch?

Before Asyncify adds its suspension code, Binaryen's **Flatten** pass moves
values from the Wasm operand stack into temporary local variables. The existing
DWARF updater moves code addresses, but does not update those variable
locations. A debugger can therefore look for a value on the operand stack even
though it now lives in a local.

Resuming a function creates another problem: Asyncify restores only the locals
needed by subsequent execution. A variable whose last use was before the
suspension may still be visible in the C scope, but its value is no longer
available. Reporting the new call frame's zero-initialized local is incorrect.

The Wasm can run correctly, and the DWARF can pass structural verification,
while the debugger displays wrong values. These patches repair the descriptions
where possible and mark values **unavailable** where they cannot be proved.

## Which Binaryen build is this?

The source is **Binaryen 133**, commit
`fba5005a132d85d578be5bd010e0487ed8e2ce3e`, pinned under
`[toolchain.binaryen-src]` in [`inputs.lock`](../../inputs.lock).
The build applies these patches in order:

| Patch | Purpose |
| --- | --- |
| [`upstream-scope-ranges.patch`](upstream-scope-ranges.patch) | Repairs invalid code ranges for functions and variable scopes. Backported from upstream [#8964](https://github.com/WebAssembly/binaryen/pull/8964), commit `06f14fc522372ed89e2b350ca0948eb34006d981`. |
| [`value-provenance.patch`](value-provenance.patch) | Tracks where values move during Flatten, checks whether they remain available through Asyncify, and repairs variable locations and source-line gaps. |

Release builds still use the unpatched Binaryen 133 distribution. Keeping the
same base version lets the tests check that these patches change debug
information, not executable sections, without also introducing a toolchain
upgrade. The newer upstream scope-range fix alone does not repair variable
locations. Its backport here includes source changes and the original
license/attribution, but not its upstream tests.

## Build

Run from the repository root, with the usual game inputs available:

```sh
psxw build bof4 --debug
psxw build-web bof4 --debug --moddable
```

Here `bof4` is Breath of Fire IV. The second command builds a web bundle with
script-mod support.

The first debug build of a threaded game builds the patched tool automatically.
**Docker is required for that initial build.** The builder downloads and
hash-checks the source, creates a compiler image from
[`build.Dockerfile`](build.Dockerfile), and compiles a static `wasm-opt` with
network access disabled. The image uses the base image and Debian package
snapshot pinned in `inputs.lock`'s `[oracle]` section.

The result is cached at `.toolchain/binaryen-dwarf-<hash>/bin/wasm-opt`, alongside
`build.log` and `provenance.json` (input and binary hashes). Changes to the
source pin, patches, Dockerfile, build script, base image, or package snapshot
select a new cache directory. The implementation is
[`dwarf_binaryen`](../../crates/psxw-inputs/src/toolchain.rs).

## Run the tests

All commands below run from the repository root.

### Integration test: no game disc or browser required

Requires Docker, the project's pinned clang toolchain, and Node.js:

```sh
cargo test --locked -p psxw-link --test threaded_dwarf -- --ignored --nocapture
```

This builds [`tests/fixture.c`](tests/fixture.c) at `-O2 -g` and checks:

- Every non-custom Wasm section matches unpatched Binaryen after a no-pass
  roundtrip, Flatten, and Asyncify restricted to selected functions.
- DWARF passes verification, including after the project's Rust linker combines
  the guest code with the runtime.
- Variables resolve to the expected values in V8 after actual suspension and
  resumption. Branch and loop cases run with inputs 0, 1, 5, and 100.
- Missing values are reported as unavailable, including a local no longer
  needed after suspension. Source lines do not gain extra statement markers.
- Local indices above 127, an alternative clang instruction selector, and
  known malformed compiler descriptions are exercised by separate variants.

It is an opt-in test because it builds Binaryen. Missing prerequisites fail the
test rather than silently skipping it. Outputs go to `out/threaded-dwarf-tests/`.

### Browser test: C/C++ variable inspection

Run the integration test first to create the fixture modules. Then provide a
Chromium browser, an unpacked **C/C++ DevTools Support** extension, and Puppeteer:

```sh
export PSXW_CHROMIUM=/path/to/chromium
export PSXW_DWARF_EXTENSION=/path/to/unpacked/extension
export PSXW_PUPPETEER_MODULE=/absolute/path/to/puppeteer/lib/esm/puppeteer/puppeteer.js
node toolchain/binaryen-dwarf/tests/browser.mjs out/threaded-dwarf-tests/fast/merged.wasm
node toolchain/binaryen-dwarf/tests/browser.mjs out/threaded-dwarf-tests/fast/merged.wasm typed
```

`PSXW_PUPPETEER_MODULE` is optional if Node can import `puppeteer` normally. The
`typed` case checks structures, pointers, floating-point values, and 64-bit
integers. These tests use a fresh browser profile, HTTP-served C sources, C
source breakpoints, and the real DevTools C-expression evaluator—not a custom
DWARF evaluator.

### Full-game browser tests

These additionally need local game discs, the corresponding game builds, a
built web renderer (`pkg/`), and the replay files named by the test configs in
`out/oracle/replays/`:

```sh
export PSXW_DWARF_DISC_DIR=/path/to/bof4/disc-directory
node toolchain/binaryen-dwarf/tests/game-browser.mjs toolchain/binaryen-dwarf/tests/bof4.json
node toolchain/binaryen-dwarf/tests/game-browser.mjs toolchain/binaryen-dwarf/tests/bof4-mod.json
export PSXW_DWARF_DISC_DIR=/path/to/btn/disc-directory
node toolchain/binaryen-dwarf/tests/game-browser.mjs toolchain/binaryen-dwarf/tests/btn.json
```

`btn` is Harvest Moon: Back to Nature; it does not use Asyncify and serves as a
control. Set `PSXW_DWARF_RENDERER_DIR` to select another renderer `pkg/` directory.
The browser environment variables from the preceding section still apply.

The tests check every breakpoint stop and compare C expressions with independent
reads of emulated memory, thread records, and stack pointers. They count correct,
incorrect, and unavailable results separately. The script-mod case also requires
both entry and exit hooks to run through the production mod loader.

Previously tested with Chromium 151.0.7922.137 (Brave), Puppeteer 24.31.0, and
C/C++ DevTools Support 0.2.5854.1. The extension archive's SHA-256 is recorded in
[`acceptance.json`](acceptance.json). Browsers, extensions, discs, and
game-derived source are not bundled here.

## How the repair works

1. **Track the original value.** While reading Wasm, record which instruction
   produced each stack value referenced by DWARF. Track only the code ranges
   that need this information.
2. **Record where it moves.** When Flatten stores that value in a temporary
   local, record the local and the assignment that establishes its value.
3. **Check every path to a location.** Analyze the transformed control-flow
   graph, including branches and loops. Distinguish normal execution,
   suspension ("unwind"), and resumption ("rewind"), using Asyncify's own
   information about which calls can change state. A location is retained only
   where the analysis proves it valid. Restoring an original local can make it
   available again; restoring a temporary is not, by itself, proof that it
   contains the recorded producer's value.
4. **Rewrite the DWARF.** Replace stack locations with proven local locations
   and split their valid code ranges as needed. Decode full expressions,
   including multi-byte local indices and supported multi-part values. Rebuild
   location lists and their references when their sizes change.
5. **Leave generated code unmapped.** Preserve source-line-zero entries, which
   mean "no source line," and add gaps around transformation-generated code.
   Do not turn every instruction belonging to one source statement into a new
   breakpoint stop. A C line can still have several separate code ranges.

This is not a general solution for every Binaryen pass or DWARF format. The
location-rebuilding path targets **wasm32 DWARF 4**; unsupported compilation-unit
formats fail explicitly. Unproved stack values, multi-value/control-flow
results, and unsupported expressions become unavailable rather than guessed.
Simple frame-base expressions—the descriptions used to locate stack-backed C
variables—stay inline because the tested DevTools extension cannot evaluate
their Wasm form inside a location list. Their dependent variables still have
restricted validity ranges.

## Related fixes outside these patches

Binaryen cannot fix debug information that was already wrong when clang
produced it. The project also handles several known input problems:

- **Reused argument locals.** Clang can describe an argument as occupying one
  local for the whole function even after reusing that local. The Rust DWARF
  importer removes these unsafe descriptions, without dropping all parameters.
- **Lost assignments.** The default clang instruction selector can leave
  generated game variables described as constant zero after they change. Game C
  uses `-O2 -g -mllvm -fast-isel=true` instead. Because this selector can generate
  different code with and without `-g`, release and debug builds share the same
  compiled objects; release linking drops the DWARF. This is not an `-O0` build.
- **Incomplete values.** The importer rejects unsupported partial descriptions
  of structures and 64-bit values that the debugger might otherwise display
  as complete, zero-filled values. Address-backed structures and supported
  complete descriptions are retained. Compile the fixture with
  `-DLLVM_INPUT_REPRO` to reproduce the partial-description cases.

The Rust linker also excludes extra instructions introduced by its own guest
code rewrites from variable-location and source-line ranges. Moving only the
endpoints would incorrectly extend old descriptions through the new code.

These are conservative workarounds, not a complete LLVM debugging fix. The
argument reproducer is retained in the fixture; no upstream issue was filed for
it. The debug pipeline still uses the Rust module merger and does **not** run a
final `wasm-opt -O2` pass. The patches do not change which functions Asyncify
instruments or which import triggers suspension (`psx.bios`).

## Recorded results

**Historical, not a certification of the current patch.** These results were
recorded at project revision `16dafb7`, with patch SHA-256 `9f4f07ce…`, before the
2026-10-30 fixes for locals lost after resumption, statement markers, and the
Docker builder. Full records and hashes are in
[`acceptance.json`](acceptance.json); reruns remain on the
[open-task list](../../plans/open-items.md#debug-builds-of-threaded-games-d53-d57).

- **Standalone fixture:** after two suspensions and the Rust merge, `resumed`
  correctly displayed 198 and 211; the function returned 508. The branch/loop
  case returned 794 after three suspensions. Mixed-type tests recovered
  structures, pointers, floats, doubles, and 64-bit integers.
- **Game browser checks:** bof4 reported 24 correct / 8 unavailable, its
  script-mod variant 8 / 4, and btn 28 / 7, with no incorrect results. These
  counts are not all independent tests of repaired variable locations:
  constants and direct memory reads contribute to them. Bof4's checks cover C
  generated from machine code, not its empty decompiled function stubs.
  Stronger checks with values that change between stops are still needed.
- **Debug information retained:** 557,064 of 667,169 bof4 variable descriptions
  and 37,421 of 43,254 btn descriptions had locations. This measures retention,
  not correctness or availability at every instruction. Full DWARF verification
  passed for both games and the script-mod build.
- **Execution comparisons:** release/debug replay reports matched for bof4's
  `title-to-field` (26,000 frames), btn's `title-to-gameplay` (6,000), and
  `card-load` (3,000). Only the WAV pathname was excluded; timing, memory,
  rendered video-memory hashes, and audio matched, including WAV bytes.

Debug metadata is large. With warm caches, the recorded bof4 build took about
2m54s and produced a 208 MB module; the script-mod web build took 7m10s and
produced 580 MB, of which 527 MB was DWARF. The latter peaked at about 18 GiB of
memory. These are local measurements, not cold-build estimates, and there is no
unpatched baseline isolating the repair's overhead.

Optimized-out variables remain normal. Debugger reads through some PlayStation
address aliases and scratchpad addresses remain unsupported (see the debug-build
section of [`docs/design.md`](../../docs/design.md)). Also, casting an unavailable
pointer can make the tested debugger display zero: check the named variable's
availability rather than treating that cast as a valid value.
