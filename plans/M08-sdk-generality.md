# M8 — SDK generality

**Status:** in progress (tasks 1–4)
**Depends on:** M2. Parallel with M4–M7. **Unlocks:** M9.
**Design refs:** design §4 (the translated SDK cache), §5 (devices, BIOS,
adapters keyed by object identity; layout generation); decisions D12–D15,
D8–D11.

## Goal

Make the SDK side work for any PsyQ version and any game, not just btn's
PsyQ 4.7 set:
- **Adapters keyed by object identity,** so games that mix versions work.
- **Struct layouts generated from each SDK version's headers.**
- **A reusable translated-SDK cache.**
- **psyq-decomp's C used wherever it provably matches.**

## Context

- M1's hardware-access report classifies SDK functions; M2 and M4–M7 wrote
  adapters for btn's PsyQ 4.7 objects.
- **psyq-decomp facts** (pinned commit `302b6ae`):
  - per-member sources at `src/libraries/<lib>/<N-member>/<Function>.c`;
    functions still asm use `INCLUDE_ASM`;
  - C coverage of the members btn links is low: 13% of guest-library
    functions (libsnd, libgte, libmath, libgs, libc) and 69% of
    libgpu/libcd;
  - its SDK headers and original libraries (`library-input/`) are
    gitignored, so they aren't in the snapshot.
- **Original PsyQ objects for other games** need the user's PsyQ SDK copy
  (`.LIB` files plus headers), with its path in `psxw.local.toml`. That's a
  human-needed input for this milestone if a second SDK version is
  involved.

## Scope

**In**
- **Object identity:**
  - identify each linked SDK member in a game image, using the splat `o`
    subsegments where present, otherwise signature matching against a
    pinned PsyQ signature database;
  - the key is a relocation-insensitive hash of the member's code and data
    (fields that get relocated are masked).
- **Adapter registry:**
  - adapters declare `(library, function, validated object hashes)`
    through an attribute macro;
  - the linker resolves each hardware-touching function to an adapter for
    that exact object;
  - if none exists, it translates the function and cuts at the next lower
    function that has an adapter, ultimately at BIOS calls;
  - failing that, it errors, listing the objects that need adapters.
- **`boundary/psyq.toml`:** version- and object-keyed policy with
  per-object overrides, replacing `psyq-470.toml`.
- **Layout generator:**
  - for each SDK version's headers, generate Rust accessor modules for the
    structs adapters touch (`DRAWENV`, `DISPENV`, `CdlLOC`, `SpuVoiceAttr`,
    …), with field offsets and sizes computed by clang for wasm32 (which
    M0 proved equals o32);
  - each generated module is cross-checked with the M0 layout probe
    against a reference compiler where one is available;
  - adapters use these accessors, never hand-written offsets.
- **Translated-SDK cache:**
  - a relocatable mode for `psxw-lift`: translate ELF relocatable members
    (from PsyQ `.LIB` converted with a pinned converter, run in Docker)
    with symbolic references instead of absolute addresses;
  - cache keyed by member hash in `.inputs/sdk-cache/` (local only);
  - the linker links cached members into games that use them.
- **psyq-decomp C, per function:**
  - snapshot psyq-decomp at its pin;
  - compile each C function (no `INCLUDE_ASM`) with `psxw-cc`, using the
    user's PsyQ headers or the game repo's tracked SDK headers, recorded
    in `game.toml` or `psxw.local.toml`;
  - difftest it against the translated original function *as placed in
    the game image*, matched by library, member and function name;
  - a passing function becomes the `c` tier for that game; the rest stay
    translated;
  - no member-identity bookkeeping is needed, because the differential is
    the proof.
- **Generic per-object adapter conformance:** the harness from M4/M6
  parameterized by object hash, run for every adapter and every object it
  claims.
- **Q2:** decide whether to add a register-level SPU fallback for unknown
  libspu versions. Record the decision in `decisions.md`.

**Out:** writing adapters for SDK versions no target game uses. That
belongs to whichever milestone first needs them (M9).

## Tasks

1. [x] Object identity and hashing; pin the signature database (source
   commit) in `inputs.lock`; an identity report per game.
2. [x] Adapter registry macro, linker resolution, the cut-lower fallback,
   and the missing-adapter error. *(A table rather than a macro:
   `sdk_psyq::validated::VALIDATED`, (member, identity key).)*
3. [x] Migrate the boundary policy to `boundary/psyq.toml`.
4. [x] Layout generator; regenerate the accessors for btn's adapters;
   drive the conformance diffs to zero. *(Generator and accessors done;
   adapters use them. Conformance exists for libpad only, see M4.)*
5. [ ] The PsyQ `.LIB` converter in Docker, pinned; `psxw-lift`
   relocatable mode; the translated-SDK cache; link btn from the cache and
   check that the result is identical in behaviour (difftest plus
   `ramdiff`).
6. [ ] psyq-decomp C integration: per-function compile and difftest
   against the placed original; switch to the C tier where it passes; tier
   report.
7. [ ] Generic per-object conformance in `psxw ci`.
8. [ ] Decide Q2; docs; README status. *(Q2 decided: D41.)*

## Exit criteria (all automated)

1. The identity report covers 100% of btn's linked SDK members.
2. btn builds using the registry and `boundary/psyq.toml`. Every
   hardware-touching function resolves to an adapter validated for its
   object hash. Full `psxw ci btn` still passes.
3. btn linked from the translated-SDK cache passes difftest and `ramdiff`
   on `title-to-gameplay`, identical to the image-translated build.
4. Every psyq-decomp C function used passes difftest against the placed
   original. The tier report shows the counts.
5. Per-object adapter conformance passes for every (adapter, object)
   pair.

## Risks and decision points

- **The PsyQ SDK copy is a human-needed input** for objects beyond those
  embedded in the target game's own images.
- **Signature-database coverage** of older PsyQ versions may be partial.
  Fall back to splat `o` subsegments, and record gaps.
- **The relocatable translator mode** must produce the same semantics as
  image mode. The difftest-plus-`ramdiff` equality in exit criterion 3
  guards it.

## Progress log

- **2026-10-26:** task 1.
  - **The pin:** the signature database is lab313ru/psx_psyq_signatures
    at `e9e46e7`, pinned by archive sha256 as
    `[toolchain.psyq-signatures]` (`psxw toolchain fetch`).
  - **The report:** `psxw identity <game>` writes
    `out/identity/<game>.json` (`psxw-inputs::identity`).
  - **Matching:** each SDK member's placed text is matched against every
    version's signature for its object. A decomp-split piece
    (`<object>__<function>`) is matched against its function's slice.
  - **The key:** the sha256 of the text with the relocated bytes zeroed.
  - **btn:** 358 members.
    - 356 match by signature, all of them PsyQ 4.7 (fewer in older
      versions: 349 in 4.6, 36 in 2.6).
    - 2 are identified by subsegment only (`libgte/smp_00_1__OuterProduct0`
      and `__Lzc`): no version has a signature for `SMP_00_1`.
    - None fail to match.
    - 8 functions are named differently by the database (`InitCARD` vs
      the decomp's `InitCARD2`, `Snd_play` vs `_SsSndPlay`, …). That is
      naming only.
  - **A fix it needed:** splat member paths are normalized to their last
    two components (`<library>/<member>`). btn names two libgte pieces
    `build/sdk/lib/470/libgte/…`, which had broken library lookups
    (device libraries, `--original`).

- **2026-10-26:** tasks 2–3.
  - **The boundary:** `boundary/psyq.toml` replaces `psyq-470.toml`
    (`psxw-inputs::boundary`). Each library gets a policy (`adapter`,
    `device`, `guest`), with `[object."lib/member"]` overrides. The
    device libraries come from it, checked against the runtime's support.
  - **The registry:** sdk-psyq's `validated.rs` lists the 63 objects
    btn's adapters were written against and tested on, as (member, key).
    It was generated from btn's identity report.
  - **Binding:** the build binds an adapter only when the placed object's
    key is listed. Otherwise the function is translated and calls down to
    lower adapters (`unvalidated_objects` in `build.json`).
  - **The error:** the build fails if a function that touches I/O or
    kernel RAM itself is reachable from game code by direct calls that
    pass no adapter. Unreachable ones are listed in `hardware_unbound`.
  - **btn:** every adapter's object validates, and the tiers are
    unchanged. 9 functions touch hardware with no adapter, all
    unreachable: libcard `patch`, libapi `first` and libmcx internals,
    reached only through replaced entry points.
  - **Task 4:** `psxw layout <game>` probes the structs that the
    boundary's `[layout]` names (`psxw-census::layout`, the census's
    probe), keeping values only where GCC 2.95 and clang agree. It writes
    `crates/sdk-psyq/src/layout.rs`: per struct, `SIZEOF`, `ALIGNOF` and a
    constant per field. There are 10 structs from btn's PsyQ headers (RECT,
    DRAWENV, DISPENV, CdlLOC, CdlFILE and the Spu attribute structs). The
    libgpu and libcd adapters now use them; the remaining adapters will
    too as they're written.
  - **Task 5 waits** for the original PsyQ `.LIB` files, a human-needed
    input (Context). `psxw.local.toml` would name them; until then
    btn's SDK members are translated from the game image, as now.
  - **Task 6 is parked:** psyq-decomp's C names data relative to its
    member (`D_000031F0`, `RO_STR_00003064`, `func_0000046C`) in a layout
    its gitignored build defines. Placing that C in a game means mapping
    those offsets onto the member's section placements, which needs that
    layout.

## Findings
