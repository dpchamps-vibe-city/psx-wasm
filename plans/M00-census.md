# M0 — Census

**Status:** done (2026-10-25, commit `061d029`).
**Depends on:** nothing. **Unlocks:** M1, M3.
**Design refs:** design §1, §4; Roadmap "M0 result".

## Goal

Before building the linker or runtime, find out whether clang can compile
the decompiled game for wasm32, and whether struct layouts agree between
GCC 2.95 for MIPS and clang for wasm32. That's the bet behind keeping the
PSX address space (D5).

## What exists

- **`psxw toolchain fetch`** downloads and verifies the pinned wasi-sdk 34
  (clang 23.1.0) into `.toolchain/`.
- **`psxw inputs sync`** snapshots pinned repos into `.inputs/` through
  `git archive`.
- **`psxw census <game> [--only S] [--only-header S] [--no-probe]`**
  compiles every translation unit and checks it. It writes
  `out/census/<game>/<sha7>/census.json` and prints a summary.
- **Crates:**
  - `psxw-inputs`: lock, manifest, snapshot, toolchain, splat, symbols,
    address map, originals;
  - `psxw-cc`: prelude, asm normalizer, error-driven fixups, tree-sitter
    AST helpers, layout probe;
  - `psxw-link::object`: relocatable wasm object reader with name
    classification;
  - `psxw-census`: checks and report.
- **Tests:** `cargo test --workspace`. The probe's end-to-end mismatch test
  runs when `PSXW_TEST_CC1` and `PSXW_TEST_CLANG` are set.

## Results for btn @ 3a15992

- **All 1,802 TUs compile.**
- **Functions by address:** every container's total equals the decomp's
  own count (exe 462, ovl_11 1,606, …). 2,584 are C and 90 are lifted:
  - 49 `INCLUDE_ASM`;
  - 21 with real inline asm;
  - 11 GNU nested functions (outer);
  - 9 nested functions (inner).
- **References:** 10,137 resolve; none are unresolved. 9 go to host
  support (`abs`).
- **Layout probe:** 8,374 of 8,374 entries match across 253 headers.
  - 8 headers skipped: 3 are assembler-only, 5 are included by no TU.
  - 4 bitfields are unchecked.
- **Normalizations applied:**
  - incomplete-array moved (1,472) or dropped (2,132), across all TUs;
  - register binding (47), global register (1);
  - include-asm (49), top-level asm (1,802), break (1), barrier (16).

## Findings that later milestones must handle

- **Signature-mismatch thunks:** 32 (`FntPrint` ×27, `KanjiFntPrint` ×5),
  called through K&R declarations. M1/M2 need variadic o32 adapters.
- **Interior entries:** 6 symbol-file "functions" inside `func_8001A8D0`'s
  extent (`func_8001A940`…`A968`). M2 emits entry stubs if anything
  references them.
- **Tentative definitions:** 43 data addresses are defined by several TUs.
  The M1 linker must merge them, and sizes must agree.
- **Anonymous data:** 61 symbols (string literals, function statics). The
  M1 linker places them in the AUX region.
- **No C-defined initialized data in btn,** because splat put data in asm.
  Data comes from the original image at boot.
- **clang warnings worth tracking** as portability signals: 138
  `-Wint-conversion`, 97 `-Wincompatible-pointer-types`, 9
  `-Wreturn-type`, 2 `-Warray-bounds`.
- **GTE.** Functions using the GTE inline-asm macros are lifted today.
  M4 has an optional normalizer rule mapping those macros onto `psx-gte`,
  so they can compile as C.

## Follow-ups (owned by later milestones)

- Add `psxw census` to `psxw ci` (M1 creates `psxw ci`).
- The SDK census, over psyq-decomp's C, moves to M8.
- `boundary/psyq-470.toml` becomes version- and object-keyed in M8.

## Progress log

- 2026-10-25: implemented and passed; commit `061d029`.
