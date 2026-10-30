# Threaded-game debugging: retain DWARF and repair value provenance

**Status:** recommended implementation direction, supported by a deliberately
incomplete prototype; **not production-ready or enabled**.
**Requirement:** C-variable inspection is mandatory in every game. Source-line
mapping alone is not an acceptable substitute or the recommended first delivery.
**Background:** [original investigation](debug-builds-threaded-games-investigation.md),
[problem statement](debug-builds-threaded-games.md), D53/D56/D57 in
[design.md](../docs/design.md).

## Recommendation

**Keep DWARF and the existing D53 debug pipeline. Pursue a pinned repair of
Binaryen's debug metadata, starting at its reader → Flatten → DWARF writer
boundary. Do not replace variable debugging with source maps.**

There is now experimental evidence for this boundary, rather than just a
hypothesis:

- The demonstrated variable-location failure first occurs in **Flatten**, not
  the preceding no-pass Binaryen roundtrip, at the inspected source row.
- Original operand-stack value identities can be recorded while reading the
  input, associated with the temporary locals Flatten creates, and used to
  repair DWARF location expressions.
- A small experimental patch restored the broken C variables in the **actual
  Chrome C/C++ DWARF extension**, including after real suspension/resumption
  and after this project's Rust merge. Executable sections were unchanged.

This favors repairing the existing producer over designing a new exported
sidecar or implementing our own Asyncify. It does **not** establish that the
complete repair is tiny: validity ranges, synthetic execution states and
other expression forms still need proper handling.

No production code, build guard, runtime behavior, instrumentation scope or
toolchain pin changed. The earlier [source-map proposal](source-maps-all-games-recommendation.md)
is not the recommendation for this requirement.

## 1. Stage isolation: the value is lost during flattening

Used the original investigation's C fixture and pinned wasi-sdk 34 / Binaryen
133. `root(5)` calls `worker(5)` and `worker(6)`. At C line 17, `resumed`
should be 198 and 211 respectively.

The same source row was inspected through Node's V8 inspector at each stage.
PCs below are absolute file offsets for these exact artifacts:

| Stage | Breakpoint PC | Operand stack at the stop | `resumed` description | Result |
| --- | ---: | --- | --- | --- |
| Plain clang `-O2 -g` | `0xB5` | `[0, 198]` / `[0, 211]` | operand-stack slot 1 | Correct |
| Binaryen `-g` roundtrip, no passes | `0xA3` | `[0, 198]` / `[0, 211]` | operand-stack slot 1 | Correct |
| `--flatten -g` alone | `0x12F` | empty | operand-stack slot 1 | **Wrong; value is in local 12** |
| Scoped `--asyncify -g` | `0x22F` | empty | operand-stack slot 1 | **Same defect** |

All return 508. The Asyncify variant performs two real suspend/resume cycles
and restores the buffer pointer to 4104. This isolates the earliest failure
**for this variable and row**; it does not certify every roundtrip variable,
scope or line-table entry.

Source inspection explains the result. Flatten's general value-handling
case introduces a `local.set` to a fresh temporary and replaces uses with
`local.get`. The original producer expression remains in the IR, underneath
that new assignment. The missing information is the association between the
input stack value and that temporary, plus its validity—not the runtime
value itself.

## 2. A producer-side repair experiment

The [research patch](experiments/asyncify-dwarf-value-provenance.patch) applies
only to a disposable checkout of Binaryen
`fba5005a132d85d578be5bd010e0487ed8e2ce3e` (version 133).

It performs three operations:

1. **Before transformation:** record the input operand-stack producer
   expressions at instruction boundaries, using the reader's existing IR
   builder state.
2. **During Flatten:** when an original value expression is stored into a
   newly allocated temporary, record the expression, local index and
   assignment instruction.
3. **During DWARF writing:** for the supported location-list entries, replace
   `DW_OP_WASM_location(stack, index)` with the recorded local location,
   checking that the emitted assignment precedes the inspected location.

This is an actual value-provenance association. It does not guess a local by
matching runtime numbers, hardcode local 12, identify variables by their
names, or infer value locations from source-map lines.

For `resumed`, it changes:

```text
DW_OP_WASM_location 0x2 0x1, DW_OP_stack_value
                       ↓
DW_OP_WASM_location 0x0 0xc, DW_OP_stack_value
```

The experiment rewrote nine operand-stack descriptions in instrumented
fixture functions. The one remaining description belongs to the
uninstrumented `leaf`. This count is **not game-wide variable coverage**.

### Executable and structural controls

- Built the unmodified pinned source first. Its fixture Asyncify output was
  byte-identical to the installed pinned binary's output.
- After applying the prototype, **every non-custom section** in the fixture's
  Asyncify output remained byte-identical to the pinned output.
- Patched Asyncify DWARF and the subsequent Rust-merged DWARF both passed
  `llvm-dwarfdump --verify`.
- Across `root`/`branchy` and inputs 0, 1, 5, 100, the plain, pinned Asyncify,
  patched Asyncify and patched-then-Rust-merged variants matched: **eight
  input cases / 32 executions**. The Asyncify cases performed real
  suspensions and restored the buffer pointer.

Structural verification and execution equivalence alone would not prove
variable correctness; the following browser checks are the important part.

## 3. Actual C-variable inspection in DevTools

Used a fresh headless Brave browser reporting **Chromium 151.0.7922.137**, with
Google's **C/C++ DevTools Support (DWARF) 0.2.5854.1** extension. Loaded the
normal DevTools frontend, served Wasm and C over HTTP, and evaluated named C
expressions through the frontend's paused-call-frame evaluator. This used
the real extension, not a hand-written DWARF evaluator.

At `worker`'s line-17 row, after actual suspension/rewind:

| C expression | Pinned Asyncify | Patched Asyncify | Patched + project's Rust merge |
| --- | --- | --- | --- |
| `before` | 16 / 17 | 16 / 17 | 16 / 17 |
| `resumed` | **evaluation error** | **198 / 211** | **198 / 211** |
| `stack_value` | 32 / 34 | 32 / 34 | 32 / 34 |
| `*(&stack_value)` | Not sampled | Not sampled | **32 / 34** |

The unpatched extension reports an absent operand-stack entry (`No operand
with index 1`), exposed by evaluation as `Invalid primitive wasm type`.
With repaired DWARF, it displays the expected C integer values.

For the merged artifact, a breakpoint set through DevTools' **source-level
breakpoint manager on C line 17** stopped in both worker calls. `&stack_value`
was represented as `volatile int *`, and dereferencing it returned 32/34.
The final `root(5)` result remained 508, with two real suspension cycles.

A separate check at `branchy(5)`'s return-expression row, after three
suspensions through its branch and loop work:

| Expression | Pinned Asyncify | Patched Asyncify |
| --- | --- | --- |
| `saved` | 65 | 65 |
| `result` | evaluation error: absent operand | **729** |

The function returned 794 in both cases. The loop is optimized in the input;
this is not an exhaustive test of arbitrary loop/control-flow shapes.

`first` is unavailable at the worker stop even in the original clang output;
the prototype does not manufacture additional lifetime information for it.
Ordinary optimized-out values and false descriptions must remain separate
categories.

## 4. Why this patch must not ship as-is

The prototype deliberately answers a feasibility question: **do the needed
value identities survive, and can the existing writer describe their new
location?** It does not answer every correctness question.

Specific omissions:

- It handles only the simple four-byte stack-location-plus-`stack_value`
  expression, with one-byte indices. Compound expressions, pieces,
  expression attributes outside location lists and larger indices need
  general decoding/re-encoding.
- It preserves location-expression byte lengths. Binaryen's current writer
  relies on that to leave `.debug_loc` references unchanged. A complete
  repair must rebuild lists and update references when sizes/ranges change.
- The assignment-before-PC check is **not a control-flow dominance or
  lifetime proof**. Availability must be established across branches, loops,
  joins, calls and restore paths, then represented by correct output ranges.
- It covers the general Flatten spill case, not every control-flow result,
  reader-synthesized value, tuple or later replacement operation.
- It does not fix scope/range topology or remove synthetic unwind/rewind
  intervals from source-variable availability.
- It records stacks at every input instruction and scans functions while
  updating locations. That is intentionally expedient research code, not an
  acceptable unmeasured implementation for a large game. Production tracking
  should be limited to relevant DWARF locations/values and indexed by function.
- **Unsupported cases retain their old metadata.** The experiment is not
  fail-closed and must never be used to claim correct debugging of arbitrary
  code. Production must explicitly make unprovable descriptions unavailable,
  while demonstrating that useful variable inspection remains.

The newer upstream [scope-range repair #8964][ranges] is relevant work to
integrate/evaluate. Another independent issue, [#9164][line-zero], documents
Binaryen dropping DWARF line-0 rows and attributing synthetic code to preceding
source lines. Its cited check exists in the pinned source. These do not
negate the recovered values, but show why fixing only a location opcode is
not the complete debugging repair.

## 5. Two controls change the acceptance work

### Unoptimized C is not a free workaround here

The unmodified `-O0 -g` fixture **does** expose `input`, `before`, `first`,
`resumed` and `stack_value` correctly after two Asyncify suspensions. Its
variables use frame-relative memory locations and its frame-base local
survives the inspected transformation. This is a useful positive control.

But switching the whole debug build to `-O0` is not the recommendation:

- It changes D53's current compilation policy and can change loop/call shapes
  that interact with the project's guest rewriting and safepoints.
- The project maps clang's `__stack_pointer` to the guest's `$sp`.
  Extra C spills therefore consume **guest memory**, not a separate harmless
  debugger stack.
- Compiling actual bof4 lifted `exe/801395A0.c` with the project's flags at
  `-O0` added a **176-byte compiler stack frame**; `-O2` had no such frame.
  D57 records worker stacks as small as `0x200` bytes. This example is the
  startup function, not proof of a particular worker overflowing, but it
  demonstrates why unoptimized lifted C cannot be assumed safe or equivalent.

LLVM's [limited debug-code stackification change][llvm-o0] also makes clear
that unoptimized Wasm and optimized Wasm have intentionally different
variable-storage/code-generation policies. A separate unoptimized debug
mode would require an explicit design change and game-level stack/behavior
validation, not merely changing a flag and removing the guard.

### An incorrect argument location already exists before Binaryen

The extension revealed a separate baseline defect: at the plain optimized
worker's line 17, `input` displays **16/17**, although the calls passed **5/6**
and the C never reassigned the parameter. Clang describes `input` as local 0
for the whole function; that local has been reused for `before`.

This was confirmed in the input DWARF and raw runtime state, before any
Binaryen pass. The prototype neither causes nor repairs it. It is **not**
ordinary optimized-out-variable behavior, and must not be counted as an
Asyncify regression or hidden by reporting only the successful variables.

The pinned LLVM [register-coloring implementation][llvm-coloring] has
DBG_VALUE invalidation logic, but reading that code does not isolate the
cause of this case. File a separate minimal LLVM/clang reproducer and resolve
or conservatively invalidate the false description at the proper producer
boundary. The broader correctness requirement needs input-metadata controls
as well as transformation tests.

## 6. Proposed implementation boundary and gates

The original investigation left the repair boundary undecided. The new
experiment gives a reason to choose **an in-Binaryen metadata repair first**:
it has the original stack-building state, the assignments that move values,
and the emitted positions. Reconstructing that externally would duplicate
information already available at the transformation site.

Keep clang optimization policy, D57's scope, the debug window, Rust merge,
served C paths and no-final-`wasm-opt -O2` debug policy unchanged. A source map
may be a useful internal diagnostic, but is not the user-facing replacement.

The implementation should be gated by three results:

1. **Correct transformed metadata.** Replace the prototype's lexical-order
   heuristic with output validity/range analysis; handle the actual expression
   forms in game code, emitted local indices, synthetic gaps and scope
   topology. Rebuild lists/references rather than restricting indices to fit
   old bytes. Use the worker/branch fixture as a compact integration test,
   extended for the relevant control-flow and type cases. Unknown must mean
   unavailable, never a guessed value. Resolve the separate false-input
   description as an upstream-input issue.
2. **Useful C inspection on real games.** Build actual bof4 C with DWARF
   through guest rewriting, scoped Asyncify and Rust merge. Inspect named
   locals/arguments, frame-relative values, globals, structures and pointers
   before/after multiple thread switches in the browser extension. Include
   instrumented and uninstrumented functions, lifted/decompiled C, overlays
   and a script mod on a debug base. Measure correctness and availability
   separately. Hiding almost everything is not a passing result.
3. **Preserved execution.** Verify unchanged instrumentation scope and
   compare bof4 `title-to-field` and existing btn replay results, timing,
   RAM/VRAM/audio and reached-state checks. Retain btn full-DWARF coverage and
   measure build/debug metadata costs. Pin the exact accepted toolchain or
   patch revision before enabling threaded debug builds.

These are completion criteria, not claims that this research passed them.
No full bof4 debug build/replay or game-derived browser session was performed.
The standalone browser tests also did not include the project's **pre-Asyncify
guest rewrites**; that remains an explicit integration gap.

**Decision:** keep C-variable inspection non-negotiable. Invest in the DWARF
producer repair demonstrated here, not a source-map-only fallback. There is
now a working proof of value recovery, but not yet a complete all-games fix.

## Reproduction and retained evidence

Use the fixture and real-unwind host protocol from the original investigation.
Compile with the pinned clang, `-O2 -g`, and `-fdebug-compilation-dir=.` so the
browser can fetch `fixture.c`. Run separate `-g` roundtrip, `--flatten -g`,
and scoped `--asyncify -g` controls. Do not add Binaryen optimization.

To reproduce the prototype, apply the linked patch to a disposable pinned
Binaryen source checkout and build `wasm-opt` with full DWARF support. The
experiment used a native CMake Release build with `BUILD_LLVM_DWARF=ON`,
`BUILD_TESTS=OFF`, `BUILD_SHARED_LIBS=OFF`, `BYN_ENABLE_LTO=OFF`, and native
C++ optimization `-O1`; this is the tool's own compilation, **not a different
Wasm pass optimization level**.

Use `worker,root,branchy` as the onlylist and `psx.bios` as the unwinding
import. At the inspected row, the patch logs input PC 81, operand slot 1,
local 12, output PC 317. Code-section bases are `0xF2` after Asyncify and
`0x109` after the actual Rust merge, giving breakpoints `0x22F` and `0x246`.
The branchy check uses code-relative `0x5AE` / absolute `0x6A0`. Derive these
again when inputs change; the assertions are source rows and values.

The browser checks used the official extension downloaded from Google's
Chrome update service (store ID `pdcpmagijalfljmkmjngeonclgbbannb`), version
0.2.5854.1, CRX SHA-256
`f4c7d9951929acccb602e3cb0e73ece394ecafc1e2094419887d95bf54bd8fce`.
A fresh profile was used; the user's installed browser/profile was untouched.

Disposable source, `patched-wasm-opt`, `browser-dwarf.mjs`, inspector results,
DWARF dumps and execution comparisons are in this session's scratchpad under
`dwarf-repair/`. The temporary build directory and build-tool environment
were removed after testing to free space. The research patch is retained
with this report so the value-tracking experiment does not depend on
preserving that temporary checkout.

[ranges]: https://github.com/WebAssembly/binaryen/pull/8964
[line-zero]: https://github.com/WebAssembly/binaryen/issues/9164
[llvm-o0]: https://github.com/llvm/llvm-project/pull/136510
[llvm-coloring]: https://github.com/llvm/llvm-project/blob/895aa2c896ada719451be2e3673c83da8ddf1141/llvm/lib/Target/WebAssembly/WebAssemblyRegColoring.cpp
