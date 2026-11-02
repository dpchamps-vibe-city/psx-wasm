# A — Explicit activation with the existing pointer ABI

**Status:** implemented; subset acceptance passes against the live reference.
The full corpus stays red on the legacy findings, as permitted at A. Remaining
milestone-C obligations are listed in every explicit build report.
**Depends on:** no earlier milestone. Implement the contracts in the
[plan README](README.md); no separate activation approval is required.

## Deliverable

Run real loading operations through production compilation and publish explicit
bindings. A saved guest pointer P must call A, then B, then A as those ranges are
loaded. Preserve original pointer values, data layout and o32 boundaries. Supplied
C remains the executable implementation, including source edits and debug locations;
missing binding instrumentation must not cause an automatic lift selection.

A first covers complete, quiescent publication from resident loading code. This
is an implementation increment, not the final execution domain: partial loads,
unknown writes and active replacement remain required C work, not unsupported
positive cases.

## Input schema to implement

Add `DispatchSection` to `GameManifest`, with `schema_version = 1`,
`require_explicit = false`, `placements`, `writers`, `rules`, and
`c_continuations` (the registration format specified in milestone C, default empty).
Unknown fields and duplicate IDs are errors. Absence of this section retains the
reported migration path until milestone C removes legacy production selection.

Each placement has `id` and `container`. Its source segments, destination layout
and text/data boundaries come from the existing container metadata. Do not copy
function inventories into the manifest. Additional compiled placements are
separate input containers; a runtime destination must match the selected placement.

A writer summary uses the same operation-site fields as a rule and a
`destinations = [{ start = <u32>, end = <u32> }, ...]` list of half-open possible
write ranges. Use inferred bounds first; pinned declarations supply bounds that
analysis cannot establish. Report which facts are inferred versus declared and
validate them against known writes. A writer proven/declared disjoint from every
executable placement needs no activation instrumentation. Intersecting writers
need rules covering their code-changing effects. An unbounded writer without a
summary produces `binding.missing_writer` in strict mode; do not add global store
trapping or silently trust that its writes are data-only.

Each rule has this fixed shape:

| Field | Meaning |
|---|---|
| `id` | Unique rule ID for diagnostics/provenance |
| `container` + `function`, or `container` + `pc`, or `boundary` | Exactly one operation site: a qualified function, original instruction address, or registered native software boundary |
| `event` | `enter`, `return`, or `after-write` |
| `c_marker` | Optional C identifier identifying an explicit source event site when an original-PC rule cannot be mapped to C |
| `effect` | `begin`, `invalidate`, `publish`, or `end` |
| `placement` | Literal placement ID, `selector`, or `request` |
| `selector`, `cases` | For `selector`: scalar value source and exhaustive declared value-to-placement mapping |
| `destination`, `length`, `source_offset` | Scalar value sources for the affected range; source offset is in the container's source image |
| `success` | `always`, `zero`, or `nonzero`; the latter two test the operation result at `return` |
| `request` | `call` for synchronous invocation state, or a scalar key for a request shared across asynchronous boundaries |
| `request_group` | Required for keyed requests; the shared static namespace of their begin/write/end rules |

Scalar value sources are finite forms: `const:<u32>`, `arg:<index>`, `return`,
`global:<qualified-symbol>` (read one u32), and `field:<arg-index>:<byte-offset>`
(read one u32 through that argument). `after-write` also provides `write-address`
and `write-size`. For `source_offset`, `placement` means derive the source offset
from the destination and the selected placement's segment mapping; reject a range
without an unambiguous mapping. Arguments are original o32 argument words, including
stack arguments, captured at entry through existing marshalling; `return` is the
first o32 return word. Field/global reads occur at the selected event using guest
data addressing and captured argument pointers. Success predicates test
that return word. No arbitrary expressions, executable plugins or game-specific
Rust callbacks are part of the schema. Validate indexes, types, ranges and event
availability before code generation.

`begin` captures placement and allocates a compiler-private load-instance ID.
`request = call` selects the innermost live declared begin-context in the current
guest thread, so nested copy/write operations can share their outer load's instance.
Contexts nest and survive suspension. Keyed requests use a private map keyed by
`(request_group, scalar key)` and remain live until `end`; reusing a still-live key
within that group is an error. `publish` can inherit
placement/instance from that record. `end` removes the corresponding record and
never rolls back writes. A standalone synchronous `publish` with a literal/selected
placement and no begin-context creates its own instance. `invalidate` needs only
site/destination/length and never asserts an image identity.

Example for a synchronous loader whose arguments are selector, destination and
length, returning zero on success:

```toml
[dispatch]
schema_version = 1
require_explicit = false

[[dispatch.placements]]
id = "slot-a"
container = "module_a"

[[dispatch.placements]]
id = "slot-b"
container = "module_b"

[[dispatch.rules]]
id = "load-completed"
container = "resident"
function = "load_module"
event = "return"
effect = "publish"
placement = "selector"
selector = "arg:0"
cases = { "0" = "slot-a", "1" = "slot-b" }
destination = "arg:1"
length = "arg:2"
source_offset = "const:0"
success = "zero"
request = "call"
```

For a C write site without a verified compiler mapping, set `c_marker` and generate
`void psxw_binding_site_<c_marker>(uint32_t destination, uint32_t length,
uint32_t source_offset, uint32_t selector, uint32_t request_key)` in
`psxw_binding.h`. The authored C calls it immediately after the described write.
Its arguments are the normalized event values, not the enclosing function's o32
arguments; check them against literal/placement constraints and the rule's cases.
The original-PC path still instruments lifted code for that rule. Never execute
that original instruction stream instead of C to obtain a convenient write site.
A required C site with neither a verified mapping nor its declared marker produces
`binding.missing_c_write_site`, identifying the source function/rule and declaration
to add. Ordinary complete-operation C wrappers do not need markers.

This success-only summary is valid only when no overwritten code can be observed
before completion and failure occurs before writes. Otherwise use write-site
invalidation/publication rules; C implements their complete range semantics.
Do not apply this example indiscriminately to asynchronous or partially failing
loaders. Rule facts describe original operations; they do not replace them.

Parse the complete schema in A. Implement synchronous return publication and
invalidation first. A rule requiring an effect/event not implemented yet fails
with `binding.unimplemented_rule`, naming the rule; C completes it. Never ignore
an accepted rule or reinterpret request initiation as completion.

## Source seams

- `crates/psxw-inputs/src/{game,map}.rs`: schema and qualified resolution.
- `crates/psxw-build/src/game.rs`: binding plan, normal preparation and reports.
- `crates/psxw-lift/src/{program,graph,emit}.rs`: site/dependency information.
- `crates/psxw-link/src/{game,rewrite}.rs`: event wrappers, calls and tables.
- `crates/psxw-build/src/dispatch_conformance/`: fixture metadata and shared
  production preparation. No new compiler framework or crate.

## Tasks, in order

### A1. Add the common binding plan

- [x] Inventory direct calls, address-taking, original data pointers, lifted
  transfers, callbacks, native stand-ins and mod hooks. Record every current
  main-image/unique-address bypass and sampled-byte consumer.
- [x] Add per-reference categories: fixed function, changing slot, and guest
  address requiring lookup. Reuse `(container, entry)` identities; do not replace
  the compiler IR. Symbol provenance alone must not produce a fixed classification.
- [x] Parse and validate the schema above against pinned input metadata. Report
  the rule/site and missing field on failure; do not ask for a policy choice.
- [x] Build a deterministic plan containing placements, affected executable
  intervals, entries, initial availability, rules and reference classifications.
  Include unique entries and main-image overlaps, not only duplicate addresses.
- [x] Initialize only boot-resident ranges. Preserve unrelated intervals. Describe
  exact write extents so a shorter load does not erase an untouched old tail.

### A2. Generate binding updates and instrument operations

- [x] Generate private publication helpers in the linked game module. Reuse the
  current guest-address table for A and its existing o32/wrapper entries. No host
  module selector, standalone runtime crate or new scheduler is needed.
- [x] Route affected C direct calls, lifted direct/tail calls and pointer calls
  through the same binding plan. Remove main-container exemptions for those
  ranges. Preserve proven stable direct calls elsewhere.
- [x] Wrap declared operations in both C and lifted code, preserving arguments,
  returns and stack state. Capture values needed after the call. Publish on the
  declared event and predicate, not merely whenever the operation is entered.
  Keep the C body and its source locations; use verified C sites or the explicit
  marker interface above, never replace a C writer with its lifted implementation.
- [x] Ensure every invocation route reaches instrumentation. Insert before a
  boundary can disappear through optimization or preserve that boundary locally;
  do not globally disable inlining or optimization.
- [x] Keep the original load/copy/unpack and its data effects. Perform a completed
  publication without a callback/safepoint between its binding writes.
- [x] Reject unknown selector values and invalid source/destination ranges without
  publishing a guessed entry. Keep rule errors distinct from original guest faults.
- [x] Initialize/reinitialize bindings on boot/reset using the same generated plan.
  A compiled but unloaded function must not become callable by having a unique entry.

### A3. Preserve translation assumptions and existing interfaces

- [x] Retain runtime data/table reads. Prevent direct binding or expansion across
  a replaceable dependency unless it is stable for the whole relevant execution.
  Do not add data fingerprints or whole-code hashes.
- [x] Keep `GuestCall.addr`, SDK/BIOS callbacks, stand-ins and host invocation in
  their existing guest representation. They must reach A's bindings, not a second
  residency authority. Native reference encoding belongs to B.
- [x] Record the boundaries still requiring C: partial writes, active continuations,
  actual runtime merge and mod residency. Treat them as unfinished tasks with the
  README's fixed semantics, not unanswered contract questions.
- [x] Extend versioned build/conformance reports with rules, placements, fixed/slot
  classifications, available entries and all remaining legacy consumers. Reject
  malformed explicit-region facts instead of falling back to code probes.

### A4. Validate through production

- [x] Add `D05/explicit-binding-*` cases to the existing corpus. Original MIPS must
  actually load/copy modules; compiler facts use the same schema as real inputs.
  The host must not inject an activation answer between guest checkpoints.
- [x] Use one lifted and one mixed C/lift case covering A → B → A, unchanged saved
  pointers, direct/indirect calls and a second unaffected region. Include a
  failure-before-write control. Avoid a Cartesian product of tiers and variants.
- [x] Add focused schema-error checks and reuse existing D02/D03 dependency/routing
  controls. Identify which cases exercise explicit bindings versus the legacy path.
- [x] Add a small deliberately edited C control with a result/effect absent from
  its original MIPS. Verify it survives activation and the emitted implementation
  remains C. Judge that edit against its authored-source expectation, not the old
  binary; keep unchanged reference fixtures separate.
- [x] Compare matching-code fixtures in linked and production-optimized Wasm against
  authored observations and the live original-MIPS reference. The deliberately
  edited C control instead uses its authored-source expectation. Inspect covered
  paths for absence of resident-word selectors; behavioural agreement alone could
  hide legacy use.
- [x] Suppress one activation update in a disposable mutation and verify the saved
  pointer case fails. Restore the implementation and retain the evidence.

## Exit criteria

- [x] Real operations drive generated publication through normal preparation,
  linking and optimization; the fixture runner does not bypass the mechanism.
- [x] Required subset observations match, guest pointer values remain unchanged,
  and covered paths do not read code words to select implementations.
- [x] Bad facts fail explicitly; legacy coverage is listed; remaining milestone-C
  obligations stay visible. Supplied C bodies, edits and debug locations survive
  instrumentation; there is no automatic C-to-lift fallback.
- [x] Design/report changes describe the implemented subset, not completed C.

Commands once the new fixture group exists:

```sh
cargo test -p psxw-inputs --lib
cargo test -p psxw-build --lib
cargo test -p psxw-difftest --test link_passes
PSXW_DISPATCH_CASE=D05/explicit-binding cargo test -p psxw-build --test dispatch
cargo run -p psxw-cli -- conform dispatch --case D05/explicit-binding
cargo run -p psxw-cli -- conform dispatch
```

Keep subset and full reports separately. The full command can remain red at A;
its failures are not milestone-wide success. Tool-skipping tests do not substitute
for the required CLI/reference run.

## Progress log

- 2026-11-01: implemented A.
  - Schema: `psxw_inputs::dispatch` parses the complete `[dispatch]` section
    (placements, writers, rules, `c_continuations`), rejects unknown fields and
    duplicate IDs, and validates value sources and event availability, naming
    the rule and field.
  - Plan: `psxw_build::binding` resolves facts against the address map and
    tiers; shared by `psxw build` and the fixtures. Every entry in a placement's
    text is a slot; boot binds only the main image's entries.
  - Generation: `psxw_link::binding` emits the lookup, `binding_invalidate`,
    one `binding_publish:<placement>` per placement and an `operation:` wrapper
    per instrumented function. The dispatch table is the binding; a global per
    slot records which candidate is bound. Rule errors call the new
    `psx.binding_fault` (ABI 19), distinct from guest faults.
  - Reports: `build.json` schema 2 and conformance schema 2 carry placements,
    rules, writers, slots, entry and reference classifications, legacy
    consumers and the unfinished C obligations; each conformance variant names
    its `explicit` or `legacy` path.
- First-pass evidence (superseded by the completion entry below):
  - `cargo test -p psxw-inputs --lib`: 16 passed. `cargo test -p psxw-build
    --lib`: 4 passed. `cargo test -p psxw-difftest --test link_passes`: 8 passed.
  - `PSXW_DISPATCH_CASE=D05/explicit-binding cargo test -p psxw-build --test
    dispatch`: 7 variants, 0 failed.
  - `psxw conform dispatch --case D05/explicit-binding`: pass, 3 execution and
    4 safety, 0 failed, 0 unresolved. The live reference
    `emu-beetle-7fea57a-openbios-8a86b7e-runner-1` ran the lifted, mixed and
    unknown-selector variants' supported checkpoints; all agree. Kept apart
    from the full report in `out/conform/dispatch/a-subset/`.
  - `psxw conform dispatch` (full): 58 variants: 37 execution, 6 safety, 14 failed, 1 unresolved (exit 2; kept in `out/conform/dispatch/a-full/`). The failures and the unresolved
    variant are the recorded legacy baseline set; no legacy variant changed.
  - Mutation (scratchpad, restored): skipping `slot-b`'s publication fails
    `D05/explicit-binding-lifted` from step 10; the saved pointer returns
    0x103 (stale A) instead of 0x203.
  - `psxw build btn` (no `[dispatch]`, so the legacy path) links and merges with ABI 19: 2,604,657 bytes against the recorded 2,604,299, the difference being the new import and its runtime export. Its `build.json` reports 17 sampled-word addresses and 2,178 overlay entries with no residency check. `psxw run btn --frames 300 --check no-trap` passes.

- 2026-11-01, completing A. The first pass marked A done without four of its
  requirements; they are added here:
  - Writer bounds are inferred first. The planner walks the writer's call tree
    for stores at constant addresses, reports bounds as inferred or declared,
    and fails a declaration that leaves out a known write. `destinations` is
    now optional: a writer analysis bounds needs none.
  - `c_marker` sites: the census (and the fixture pipeline) generates
    `psxw_binding.h` on the C include path. Authored C calls
    `psxw_binding_site_<marker>(destination, length, source_offset, selector,
    request_key)` after its write; the linker binds it to a generated
    `binding_site:<rule>` that checks the values like any publication. A write
    rule in supplied C without a marker, or whose C never calls it, fails with
    `binding.missing_c_write_site`.
  - Argument words, `return` and success predicates are checked against each
    operation's signature in the planner, before code generation.
  - A debug link of every explicit fixture with C checks that each C
    subprogram's DWARF starts in its own body, never in a generated wrapper.
- Completion evidence (worktree `main-wt-5`):
  - `cargo test -p psxw-inputs --lib`: 17 passed. `cargo test -p psxw-build
    --lib`: 4 passed. `cargo test -p psxw-difftest --test link_passes`: 8 passed.
  - `PSXW_DISPATCH_CASE=D05 cargo test -p psxw-build --test dispatch`: 14
    variants (4 legacy, 10 explicit), 0 failed.
  - `psxw conform dispatch --case D05/explicit-binding`: pass, 4 execution and
    6 safety, 0 failed, 0 unresolved; the live reference ran the lifted, mixed,
    `c_marker` and unknown-selector variants' supported checkpoints, and all
    agree. Kept in `out/conform/dispatch/a-subset/`.
  - `psxw conform dispatch` (full): 61 variants: 38 execution, 8 safety, 14 failed, 1 unresolved (exit 2). The failures and the unresolved
    variant are the recorded legacy baseline set. Kept in
    `out/conform/dispatch/a-full/`.
  - Mutation re-run (scratchpad, restored): skipping `slot-b`'s publication
    fails all four positive explicit variants, the `c_marker` one included.
  - `psxw build btn` (no `[dispatch]`) still links and merges at 2,604,657 bytes, and `psxw run btn --frames 300 --check no-trap` passes.

- 2026-11-01, review fixes. An independent review reproduced five defects. Each
  now has a regression fixture, and each fixture fails when its fix is reverted
  (scratchpad mutations, restored):
  1. A declared operation written `inline` was inlined into its C callers, which
     skipped its wrapper. Declared operations now keep their call boundary:
     `hook_variant` drops `inline` and makes them weak and noinline like other
     functions. The C variants' loader is `inline` and called from C `load_c`;
     reverting fails the mixed and edited-C variants.
  2. A C operation's by-value struct words were read after its body ran. Every
     C argument word a rule reads is now captured at entry. The C loader takes
     its request by value and the edited C overwrites it; reverting fails the
     edited-C variant.
  3. A `c_marker` call ignored the rule's literal values. Values a rule
     declares as `const` or `global` must now equal the call's
     (`binding.marker_mismatch`). Reverting fails `explicit-binding-marker-mismatch`.
  4. A marker call anywhere in the object satisfied the site check. It must
     now be in the function holding the write. Reverting fails
     `explicit-binding-marker-elsewhere`.
  5. An empty invalidation inside a footprint unbound it. Empty updates now
     change nothing; the positive variants clear zero bytes inside A, and
     `explicit-binding-cleared` checks a real clear unbinds. Reverting fails
     all four positive variants.
- Review-fix evidence: `psxw-cc` 21, `psxw-inputs` 17, `psxw-build` 4 and
  `link_passes` 8 tests pass. `psxw conform dispatch --case
  D05/explicit-binding`: 13 variants, 4 execution and 9 safety, 0 failed, 0
  unresolved, with the live reference agreeing on the 6 it runs. Full corpus:
  64 variants: 38 execution, 11 safety, 14 failed, 1 unresolved (exit 2), the failures and the unresolved variant being the legacy baseline
  set. `psxw build btn` links at 2,604,703 bytes (the runtime gained the new fault cause's name) and `psxw run btn --frames 300 --check no-trap` passes.

- 2026-11-01, second review fixes. Three more defects reproduced; each has a
  regression that fails when its fix is reverted (scratchpad mutations,
  restored):
  1. Boundaries were chosen by the manifest's literal function name, so naming
     the loader by its `func_<ADDR>` alias left its C inlinable. Boundaries are
     now resolved to addresses, and any C name for that address counts. The
     same name comparison made an aliased rule miss its writer; writer coverage
     now compares addresses too. The mixed and edited-C variants name the loader
     by its alias.
  2. Only declared operations kept their boundary, so an `inline` function at a
     changing slot could vanish into another C function and skip its binding
     lookup. Every C function at a covered address now keeps its boundary.
     Module A has matching C in the C variants, its `f` `inline` and called by
     its tail `k`; reverting returns stale A from `k` after B loads.
  3. A marker's source offset went unchecked when the rule derives it from the
     placement. It must now equal the derived offset, or the publication is
     rejected (`binding.bad_destination`). The positive marker variant derives
     its offset; `explicit-binding-marker-offset` passes a wrong one.
- Second-review evidence: `psxw-cc` 21, `psxw-inputs` 17, `psxw-build` 4 and
  `link_passes` 8 tests pass. `psxw conform dispatch --case
  D05/explicit-binding`: 14 variants, 4 execution and 10 safety, 0 failed, 0
  unresolved, the live reference agreeing on the 7 it runs. Full corpus:
  65 variants: 38 execution, 12 safety, 14 failed, 1 unresolved (exit 2), the failures and the unresolved variant being the legacy baseline
  set. `psxw build btn` links at 2,604,703 bytes and `psxw run btn --frames 300 --check no-trap` passes.

- 2026-11-01, third review fix. The linker lowered a constant-length `memcpy`
  (and `abs`, `labs`, `fabs`) to an intrinsic before resolving the callee, so a
  rule on `memcpy` never ran. A guest function a rule instruments is now always
  called. `explicit-binding-memcpy` loads B through C's constant-length
  `memcpy`, which a rule publishes; reverting the fix returns stale A. Evidence:
  the explicit subset is 15 variants, 5 execution and 10 safety, 0 failed, 0
  unresolved, the live reference agreeing on the 8 it runs. Full corpus:
  66 variants: 39 execution, 12 safety, 14 failed, 1 unresolved (exit 2), the failures and the unresolved variant being the legacy baseline
  set. `psxw build btn` links at 2,604,703 bytes and `psxw run btn --frames 300 --check no-trap` passes.

- 2026-11-01, fourth review fixes:
  1. Intrinsic lowering still replaced a call to a function at a covered
     address (one named `abs`), skipping its binding lookup. Calls to changing
     slots now stay calls too. `explicit-binding-intrinsic-slot` names the slot
     function `abs`; reverting returns 1 with no effect instead of 0x101.
  2. Any rule at a writer's site counted as covering it, whatever its range.
     When every covering rule's range is constant, those ranges must now cover
     the code the writer can change. `explicit-binding-uncovered-write`: a known
     store into the slot's code, covered only by an unrelated constant range,
     fails the build. A range computed at run time can't be refuted at build
     time and still counts.
  3. Production's void-result policy replaced supplied C with its translation
     unconditionally. In a `[dispatch]` build it no longer does: each such
     function needs an explicit `[tier]` lift pin, or the build fails with
     `binding.c_replaced` naming them. Builds without `[dispatch]` keep the
     migration policy. A unit test covers both; the fixture pipeline has no
     void-result selection to exercise.
  Evidence: the explicit subset is 17 variants, 6 execution and 11 safety, 0
  failed, 0 unresolved, the live reference agreeing on the 9 it runs; reverting
  fix 1 or 2 fails its fixture. `psxw-build` 5 tests pass. Full corpus: 68 variants: 40 execution, 13 safety, 14 failed, 1 unresolved (exit 2),
  the failures and the unresolved variant being the legacy baseline set. `psxw build btn` (no `[dispatch]`, so its void-result policy is unchanged) links at 2,604,703 bytes and `psxw run btn --frames 300 --check no-trap` passes.

- 2026-11-01, fifth review fixes:
  1. `weak, noinline` kept a changing call's boundary but not the call itself:
     C declaring the slot function `__attribute__((const))` let clang merge
     two calls across a load. C that declares a declared operation or a
     function at a covered address `const` or `pure` (or C23 `unsequenced`,
     `reproducible`), anywhere in the TU, now fails with
     `binding.elidable_call`. `explicit-binding-const-callee`.
  2. Writer analysis followed a call to a changing slot into the caller's own
     container only. It now follows every implementation the slot can bind.
     `explicit-binding-callee-candidates`: A's k calls the slot, A's f writes
     nothing, B's f clears the second region's code; k's writer needs a rule.
  3. Tier selection walked successful translations only, so supplied C whose
     original didn't translate got no tier and its slot reported untranslated.
     Supplied C now takes its tier (and its binding, by its function's extent)
     either way, in production and in the fixture pipeline.
     `explicit-binding-c-untranslatable`: A's symbol file sizes f to end
     before its return's delay slot.
  4. C defining `psxw_binding_site_*` passed site validation while the linker
     instrumented only undefined calls, so the rule never ran. Any definition
     of one now fails with `binding.defined_marker`.
     `explicit-binding-defined-marker`.
  5. A constant range ending at 2^32 wrapped to zero and panicked the planner.
     Window offsets are now computed from start and length, and the schema
     rejects a constant range running past 2^32 (unit test).
     `explicit-binding-top-word` invalidates the last word and runs.
  6. Declared writer bounds and known writes compared as raw addresses. Both
     now compare as backing bytes (D5), split where a range wraps the window.
     `explicit-binding-alias-bounds` declares the pointer store's bound through
     its KSEG1 alias. Lifted stores ending at 2^32 count as unbounded rather
     than overflowing.
  Evidence: the explicit subset is 23 variants, 9 execution and 14 safety, 0 failed, 0 unresolved, the live reference agreeing on
  the 12 it runs; reverting each fix fails its fixture (the range fix by the
  reported panic). Unit tests: `psxw-cc` 22, `psxw-inputs` 17, `psxw-build` 5,
  `link_passes` 8. Full corpus: 74 variants: 43 execution, 16 safety, 14 failed, 1 unresolved (exit 2), the failures and the unresolved variant
  being the legacy baseline set. `psxw build btn` links at 2,604,703 bytes with the same tiers as before (86 adapter, 2,574 C, 727 lift: none of its supplied C has an untranslatable original), and `psxw run btn --frames 300 --check no-trap` passes.
  Not changed: a `c_marker` call's literal destination check still compares raw
  values, so the C passing an alias of a declared constant destination faults
  (`binding.marker_mismatch`) instead of publishing.

## Findings

### A1 reference inventory

| Reference | Representation | Legacy route | Covered address (explicit) |
|---|---|---|---|
| C direct call | symbol → (container, address) | direct C→C when signatures agree and the address is unshared; else an o32 entry; a shared non-main address goes through the sampled stub; main is exempt | binding lookup; never a direct C→C call |
| C address-taking, C data relocations | guest address (D6) | masked dispatch table | the same table, whose covered entries are bindings |
| Original data pointers | guest address in original bytes | masked dispatch table | the same |
| Lifted direct and tail calls (`psxw_f_*`) | (container, address) | `entry_from`: stub when shared and non-main; main exempt; unique entries direct | binding lookup |
| Lifted calls into another overlay (`psxw_d_*`) | address | stub when shared, else the only container's entry | binding lookup |
| Lifted `jalr`/`jr` (`psxw_call_indirect`) | runtime value | masked dispatch table | the same |
| Jump tables | discovered targets | `switch`, `bad_jump` otherwise | unchanged (C discovers the union) |
| Translator expansions | callee code embedded in the caller | unchecked | build error across a replaceable range (`binding.replaceable_dependency`, `binding.expanded_operation`) |
| Host invocation, `GuestCall.addr`, SDK/BIOS callbacks, `execute` | guest address | masked dispatch table; `untranslated` stand-ins | the same table, so they reach bindings |
| BIOS/kernel stand-ins | reserved addresses | reserved table slots | unchanged |
| Mod hooks, slots, synthetic entries | (container, address); synthetic addresses | heads; overlay heads through the stub | a bound entry is its hook head; mods at covered addresses are C |
| Sampled-byte consumers | code words | `Synth::OverlayStub`, `psxmod.resident`, `psxmod_loaded` | no stub at a covered address (checked per fixture build); mod residency unchanged (C) |

Bypasses before A: the main-container exemption (`entry`, `entry_from` and the
hooked-call path), unique entries (their table elements and `dynamic_entry`'s
only container), and direct C→C calls at unshared addresses. All three are gone
at covered addresses. Uncovered regions keep them and report them as
`legacy-exempt` and `legacy-unique` references.

### Implementation findings

- Replacing an operation's function index with its wrapper reaches every route
  (C direct, its o32 wrapper, the table, a hook chain's bottom, a moddable slot)
  without touching the C body. DWARF and the name section keep the body's own
  index, so source locations still name the authored C.
- Wasm cannot compare funcrefs, so a global per slot records the bound
  candidate. Invalidation unbinds a slot only when its bound implementation
  depends on a written byte.
- An implementation is bound only when one publication covers its whole
  footprint: for lifted code, its translated regions in its own container; for
  C, its extent. A footprint split across publications stays unbound until C
  composes ranges.
- Undeclared writes leave bindings stale. The mutation shows stale A, not a
  rejection. Uncovered writers are C's strict-mode coverage, not A's.
- Bindings initialize when the module is instantiated: the active table
  segment holds the boot entries, and the slot globals start at their boot
  candidates. The runtime has no in-place reset; a reset re-instantiates.
- No game declares `[dispatch]` yet. Games keep the legacy path; `build.json`
  2 reports their classification.
- Writer inference uses the lifter's register constants. A `$sp`-relative store
  is the writer's own stack frame and is reported as such. A store through any
  other register, a pointer, BIOS or kernel call, or supplied C leaves the writer
  unbounded, so its bounds must be declared. Every store at a constant address
  must lie inside a declaration.
- `psxw_binding.h` types its parameters `unsigned int`: the reference cpp the C
  tier preprocesses with has neither `<stdint.h>` nor `__UINT32_TYPE__`, and the
  type is 32 bits on both MIPS and wasm32.
- A write at a lifted instruction address stays `binding.unimplemented_rule`;
  C1 adds that original-PC path for the same rules. Supplied C's write sites
  already work through their markers.
- Contract 2's provenance in A is per slot: the bound candidate names the
  module, its footprint the range, and the placement's segments the source
  range. Load instances and records for parts of a footprint come with C's
  requests and range composition.
- Contract 3 says not to inline across replaceable dependencies. A fails the
  build where a translation does (`binding.replaceable_dependency`,
  `binding.expanded_operation`): removing such an expansion needs C's
  continuation mechanism for register-sharing transfers.
- Disc-write notifications carry ranges, not a universal activation identity.
  The schema supplies identity from software operations and input facts.
- Reusing the address table is an implementation step, not a commitment to retain
  unsafe masked callable namespaces; C implements the final namespace contract.
