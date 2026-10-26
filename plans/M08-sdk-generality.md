# M8 — SDK generality

**Status:** not started
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

1. [ ] Object identity and hashing; pin the signature database (source
   commit) in `inputs.lock`; an identity report per game.
2. [ ] Adapter registry macro, linker resolution, the cut-lower fallback,
   and the missing-adapter error.
3. [ ] Migrate the boundary policy to `boundary/psyq.toml`.
4. [ ] Layout generator; regenerate the accessors for btn's adapters;
   drive the conformance diffs to zero.
5. [ ] The PsyQ `.LIB` converter in Docker, pinned; `psxw-lift`
   relocatable mode; the translated-SDK cache; link btn from the cache and
   check that the result is identical in behaviour (difftest plus
   `ramdiff`).
6. [ ] psyq-decomp C integration: per-function compile and difftest
   against the placed original; switch to the C tier where it passes; tier
   report.
7. [ ] Generic per-object conformance in `psxw ci`.
8. [ ] Decide Q2; docs; README status.

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

## Findings
