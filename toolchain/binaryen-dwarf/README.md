# Threaded-game DWARF

The debug Asyncify path uses this **separately pinned Binaryen 133 source
build**. Release Binaryen, the D57 instrumentation algorithm, `psx.bios` as
the only unwinding import, the 2 GiB debug window, served C paths and Rust
merge are unchanged. There is no final debug `wasm-opt -O2` pass and no
source-map replacement for variable debugging.

`inputs.lock`'s `[toolchain.binaryen-src]` pins the source archive; the
patches here are applied to it in order. The upstream scope-range repair is commit `06f14fc522372ed89e2b350ca0948eb34006d981`
([#8964](https://github.com/WebAssembly/binaryen/pull/8964)), backported to
`fba5005a132d85d578be5bd010e0487ed8e2ce3e`. The second patch repairs value
provenance, ranges, expressions and synthetic source gaps. The source-only
upstream backport retains its author/license; its upstream tests are not
included in that backport.

## Producer boundary

- The reader snapshots operand producers **only at relevant input DWARF
  stack-location intervals**, including inline-expression scopes. Flatten
  records the actual producer, spill local and assignment, indexed by function.
- A forward must-definition analysis of the **emitted CFG** handles joins,
  loops and backedges. It distinguishes Asyncify's normal/unwind/rewind states,
  using Asyncify's own call-effect analysis and import/onlylist contract.
  A restored local does not establish a producer identity. A PC reachable
  during rewind as well as normal execution cannot describe a normal-only value.
- Original locals are facts too. On rewind entry none holds the program's
  value; any write (original code, or Asyncify's restore of a saved local)
  establishes it. Asyncify saves only the locals live at a suspension, so a
  variable whose last use precedes one is unavailable after it, not the
  re-entered frame's zero. A `DW_OP_fbreg` location also needs its local
  frame base to hold its value.
- Whole expressions are decoded, not matched as four magic bytes. Local indices
  use bounded ULEBs, including indices above 127. Complete pieces and ordinary
  compound operations are retained. Unproved stack values, control-flow/tuple
  results, reader-synthesized values, and unsupported branching/nested/unknown
  expressions become **unavailable**, never guessed.
- Location lists are rebuilt, with references updated after size/range changes.
  Inline function-scope expressions become lists of proven original-opcode
  intervals. Abbreviations, CU lengths, DIE references (including variable-width
  references) and aranges CU references are rebuilt too. This resizing supports
  the project's wasm32 DWARF 4; unsupported CU formats fail explicitly.
- Existing line-zero rows survive, and new line-zero gaps exclude synthetic
  code. An input row's statement flags go only to the first output opcode it
  covers: a line breakpoint stops once, not once per opcode. The Rust guest-rewrite boundary also excludes expansion interiors from
  location lists/source rows: masking a store consumes its original stack value
  before the next original instruction. Updating only its endpoints was unsafe.
  The rows it adds around an interior don't start statements either.
- Simple validated local/global frame bases remain inline: the tested DevTools
  consumer cannot evaluate a Wasm frame-base expression in a location list.
  Dependent variables still have normal-only, original-opcode validity ranges.

### LLVM input controls

The pinned clang can supply incorrect metadata **before Binaryen**:

1. `worker(input)` claims a reused local is the original argument for the whole
   function (16/17 instead of the actual 5/6). The Rust input importer omits fixed
   whole-function parameter descriptions in compiler-written locals. Valid
   ranged/compound parameters and frame bases are not blanket-deleted.
2. Its default DAG selector drops later assignments in lifted bof4 C: scheduler
   `r16/r17/r18` incorrectly become constants zero. All game C compiles at
   **`-O2`** with `-mllvm -fast-isel=true` (`psxw_cc::CODEGEN_FLAGS`), which
   keeps these records. This changes instruction selection, not the IR
   optimization level. FastISel's code changes with `-g` (437 of 1,078 sampled
   objects), so every object is compiled once, with `-g`, and release links the
   same objects without the DWARF (D53). The startup function `801395A0` still
   has no additional compiler stack frame (the rejected `-O0` control added
   176 bytes).
3. Missing aggregate pieces are materialized as zero by the tested consumer.
   LLVM can also describe only the used low bits of an i64 as the whole value.
   The importer rejects incomplete pieces (including missing tails), scalar
   constants/stack-values standing in for whole aggregates, and storage-free
   arithmetic descriptions. Declaration origins, typedefs and qualifiers supply
   the type/bit coverage; address-backed structures and complete pieces remain.
   `fixture.c` with `-DLLVM_INPUT_REPRO` reproduces the partial-piece/i64 cases.
   The real btn test also rejects `vram_clear`'s false `RECT = 0` description.

These are conservative metadata controls, not LLVM instruction changes or a
claim to have fixed every optimizing-compiler debugging defect. The fixture
is the retained minimal argument reproducer; no upstream issue was filed.

## Build

The first debug build of a threaded game builds this `wasm-opt`
(`psxw_inputs::toolchain::dwarf_binaryen`); it needs Docker. It fetches and
hash-checks `[toolchain.binaryen-src]`, builds the `psxw-binaryen-build:local`
image from `build.Dockerfile` on `[oracle]`'s pinned base image and Debian
snapshot (so the compiler is pinned too), and builds a static `wasm-opt` in it
with no network. The result goes to `.toolchain/binaryen-dwarf-<hash>/`, keyed
by the source pin, the base image, the snapshot, the build script, the
Dockerfile and the patches, with `provenance.json` and the build log beside it.
Release builds keep `[toolchain.binaryen]`.

```sh
psxw build bof4 --debug
psxw build-web bof4 --debug --moddable
```

## Reproduce acceptance

```sh
cargo test --locked -p psxw-link --test threaded_dwarf -- --ignored --nocapture
```

This compiles ordinary, wide-index, FastISel and LLVM-input-reproducer variants
at `-O2 -g`. It compares **every non-custom section** against unpatched Binaryen
for roundtrip, Flatten and scoped Asyncify; verifies DWARF; exercises real
suspension for `root`/`branchy` with inputs 0, 1, 5 and 100; inspects actual V8
state; and applies the project's actual Rust merge. The wide case forces spill
indices above 127. An unproved cross-suspension `left` must be unavailable, and
so must `dead_across`'s `dead`, last used before its suspension (the unpatched
tool still describes it there). No line gains statement rows over the input.
Missing prerequisites fail the opt-in test rather than count as a skipped pass.

For the **real C/C++ DevTools extension**, not a custom DWARF evaluator:

```sh
export PSXW_CHROMIUM=/path/to/chromium
export PSXW_DWARF_EXTENSION=/path/to/unpacked/extension
export PSXW_PUPPETEER_MODULE=/absolute/path/to/puppeteer/lib/esm/puppeteer/puppeteer.js
node toolchain/binaryen-dwarf/tests/browser.mjs out/threaded-dwarf-tests/fast/merged.wasm
node toolchain/binaryen-dwarf/tests/browser.mjs out/threaded-dwarf-tests/fast/merged.wasm typed

# The real-game cases require local discs, the corresponding builds, a web
# bundle's renderer pkg, and the existing named replay files in out/oracle/replays.
export PSXW_DWARF_DISC_DIR=/path/to/bof4/disc-directory
node toolchain/binaryen-dwarf/tests/game-browser.mjs toolchain/binaryen-dwarf/tests/bof4.json
node toolchain/binaryen-dwarf/tests/game-browser.mjs toolchain/binaryen-dwarf/tests/bof4-mod.json
export PSXW_DWARF_DISC_DIR=/path/to/btn/disc-directory
node toolchain/binaryen-dwarf/tests/game-browser.mjs toolchain/binaryen-dwarf/tests/btn.json
```

`PSXW_DWARF_RENDERER_DIR` optionally selects another built renderer `pkg/`.
Without `PSXW_PUPPETEER_MODULE`, the scripts import `puppeteer` normally. Each
uses a fresh profile, HTTP-served Wasm/C, the actual DevTools frontend, **C source
breakpoints** and its paused-frame C evaluator. Every physical stop is checked;
synthetic gaps can give one C line multiple disjoint breakpoint locations.
Game checks compare against independently read guest memory/TCB/SP and known
caller semantics, report correctness and unavailability separately, and require
all requested breakpoints. The script-mod case uses the production
`ScriptHost`/`ModLoader`, and requires both entry and exit hooks to run.

Tested: Chromium 151.0.7922.137 (Brave), Puppeteer 24.31.0, official C/C++
DevTools Support 0.2.5854.1. Its official CRX SHA-256 was
`f4c7d9951929acccb602e3cb0e73ece394ecafc1e2094419887d95bf54bd8fce`.
No browser, extension, private disc or game-derived source is bundled here.

## Accepted results

Recorded at `16dafb7` with the previous `value-provenance.patch` (sha256
`9f4f07ce…`), before the 2026-10-30 fixes: dead locals after a resume, statement
rows, the Docker builder. The fixture test above passes with the new patch; the
browser and real-game checks below are to be rerun (`plans/open-items.md`).

- Worker, after two actual suspensions and Rust merge: `before` 16/17,
  `resumed` 198/211, `stack_value` and `*(&stack_value)` 32/34. The false
  `input` description is unavailable. Result 508; buffer pointer restored to
  4104. Branch/loop result 794 after three suspensions, with `result = 729`.
- Mixed-type control: frame-relative structures/pointers, float 1.5/3.5,
  double 2.25/4.25/5.5, and i64 4294967303/8589934600/42949672967 are correct
  after suspension. Its `resumed` is legitimately unavailable at the sampled row.
- Actual bof4, through guest rewriting, scoped Asyncify and Rust merge:
  scheduler constants 0x80110000/127/0x80114a00, worker record pointer matching
  RAM, and resumed BIOS result 1. Eight physical stops across two TCBs and
  repeated switches: **24 correct checks, 8 unavailable**, no wrong values.
  The script-hook debug base adds **8 correct, 4 unavailable**, with real entry
  and exit through the normal mod chain. Bof4's decompiled C functions are
  empty stubs; useful game-variable coverage here is its lifted C, not invented
  locals in those stubs.
- Actual btn (uninstrumented, full DWARF): decompiled `scene_ptr`, global scene
  and render-context pointer, structure capacity 12000, overlay `flags`, and
  `Rand(limit)`'s 65535 argument with its independently checked C caller, and
  frame-relative `mode` / `*(&mode)` = 128 in the CD loader:
  **28 correct, 7 unavailable**. The changing rectangle falsely described by
  clang as constant zero is explicitly unavailable. Suspended frame-relative
  structures/types are additionally covered by the mixed-type fixture.
- LLVM statistics distinguish retained descriptions from sampled correctness:
  bof4 has 557,064 of 667,169 source-variable descriptions with locations; btn
  37,421 of 43,254. Parent-scope byte coverage is about 57% in each (synthetic
  gaps intentionally have none). This is not near-total variable deletion and
  is not a claim that every variable is available at every optimized PC.
- Rendered release/debug comparisons: bof4 `title-to-field` (26,000 frames),
  btn `title-to-gameplay` (6,000), and btn `card-load` (3,000). **Every report
  field matched after removing only the WAV pathname**, including timing,
  cycles, CD activity, RAM/VRAM hashes, audio and reached-state checks. WAV
  bytes also matched. VRAM checks used the actual render server, not an empty
  no-render hash map. See `acceptance.json` for the retained hashes/measurements.
- Regular bof4 instruments the same 2,951-function set (17,777 defined; one
  exception handler excluded). The scope algorithm/release tool are unchanged.
  Moddable bases have their existing extra wrapper/dispatch scope. The final
  clean pin's complete moddable-bof4 Asyncify output was byte-identical to the
  game-tested producer output, not merely its executable sections.
- Full DWARF verifies for both games and the script-mod debug base. With warm
  input/object caches, regular bof4 took 2m54s / 7,638,076 KiB peak RSS and is
  208,171,127 bytes; btn 15.47s / 711,700 KiB and 9,550,341 bytes. Moddable web
  bof4 took 7m10s / 18,469,996 KiB and is 580,335,229 bytes (526,712,214 DWARF
  bytes). These are measured local costs, not cold-build promises. The larger
  range/line metadata is intentional and remains debug-only.

Optimized-out values remain normal. Unknown locations are not reconstructed
from runtime numbers, variable names, or source lines. Scratchpad and
KUSEG/KSEG1 pointer inspection retains D53's existing limitations. A consumer
may also turn a cast of an unavailable pointer into zero; use its named-value
availability, not such a cast, as evidence of a valid location.
