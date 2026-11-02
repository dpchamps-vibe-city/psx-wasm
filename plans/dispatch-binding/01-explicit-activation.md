# A — Explicit activation with the existing pointer ABI

**Status:** not started. **Depends on:** no earlier milestone. Implement the
contracts in the [plan README](README.md); no separate activation approval is
required.

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

- [ ] Inventory direct calls, address-taking, original data pointers, lifted
  transfers, callbacks, native stand-ins and mod hooks. Record every current
  main-image/unique-address bypass and sampled-byte consumer.
- [ ] Add per-reference categories: fixed function, changing slot, and guest
  address requiring lookup. Reuse `(container, entry)` identities; do not replace
  the compiler IR. Symbol provenance alone must not produce a fixed classification.
- [ ] Parse and validate the schema above against pinned input metadata. Report
  the rule/site and missing field on failure; do not ask for a policy choice.
- [ ] Build a deterministic plan containing placements, affected executable
  intervals, entries, initial availability, rules and reference classifications.
  Include unique entries and main-image overlaps, not only duplicate addresses.
- [ ] Initialize only boot-resident ranges. Preserve unrelated intervals. Describe
  exact write extents so a shorter load does not erase an untouched old tail.

### A2. Generate binding updates and instrument operations

- [ ] Generate private publication helpers in the linked game module. Reuse the
  current guest-address table for A and its existing o32/wrapper entries. No host
  module selector, standalone runtime crate or new scheduler is needed.
- [ ] Route affected C direct calls, lifted direct/tail calls and pointer calls
  through the same binding plan. Remove main-container exemptions for those
  ranges. Preserve proven stable direct calls elsewhere.
- [ ] Wrap declared operations in both C and lifted code, preserving arguments,
  returns and stack state. Capture values needed after the call. Publish on the
  declared event and predicate, not merely whenever the operation is entered.
  Keep the C body and its source locations; use verified C sites or the explicit
  marker interface above, never replace a C writer with its lifted implementation.
- [ ] Ensure every invocation route reaches instrumentation. Insert before a
  boundary can disappear through optimization or preserve that boundary locally;
  do not globally disable inlining or optimization.
- [ ] Keep the original load/copy/unpack and its data effects. Perform a completed
  publication without a callback/safepoint between its binding writes.
- [ ] Reject unknown selector values and invalid source/destination ranges without
  publishing a guessed entry. Keep rule errors distinct from original guest faults.
- [ ] Initialize/reinitialize bindings on boot/reset using the same generated plan.
  A compiled but unloaded function must not become callable by having a unique entry.

### A3. Preserve translation assumptions and existing interfaces

- [ ] Retain runtime data/table reads. Prevent direct binding or expansion across
  a replaceable dependency unless it is stable for the whole relevant execution.
  Do not add data fingerprints or whole-code hashes.
- [ ] Keep `GuestCall.addr`, SDK/BIOS callbacks, stand-ins and host invocation in
  their existing guest representation. They must reach A's bindings, not a second
  residency authority. Native reference encoding belongs to B.
- [ ] Record the boundaries still requiring C: partial writes, active continuations,
  actual runtime merge and mod residency. Treat them as unfinished tasks with the
  README's fixed semantics, not unanswered contract questions.
- [ ] Extend versioned build/conformance reports with rules, placements, fixed/slot
  classifications, available entries and all remaining legacy consumers. Reject
  malformed explicit-region facts instead of falling back to code probes.

### A4. Validate through production

- [ ] Add `D05/explicit-binding-*` cases to the existing corpus. Original MIPS must
  actually load/copy modules; compiler facts use the same schema as real inputs.
  The host must not inject an activation answer between guest checkpoints.
- [ ] Use one lifted and one mixed C/lift case covering A → B → A, unchanged saved
  pointers, direct/indirect calls and a second unaffected region. Include a
  failure-before-write control. Avoid a Cartesian product of tiers and variants.
- [ ] Add focused schema-error checks and reuse existing D02/D03 dependency/routing
  controls. Identify which cases exercise explicit bindings versus the legacy path.
- [ ] Add a small deliberately edited C control with a result/effect absent from
  its original MIPS. Verify it survives activation and the emitted implementation
  remains C. Judge that edit against its authored-source expectation, not the old
  binary; keep unchanged reference fixtures separate.
- [ ] Compare matching-code fixtures in linked and production-optimized Wasm against
  authored observations and the live original-MIPS reference. The deliberately
  edited C control instead uses its authored-source expectation. Inspect covered
  paths for absence of resident-word selectors; behavioural agreement alone could
  hide legacy use.
- [ ] Suppress one activation update in a disposable mutation and verify the saved
  pointer case fails. Restore the implementation and retain the evidence.

## Exit criteria

- [ ] Real operations drive generated publication through normal preparation,
  linking and optimization; the fixture runner does not bypass the mechanism.
- [ ] Required subset observations match, guest pointer values remain unchanged,
  and covered paths do not read code words to select implementations.
- [ ] Bad facts fail explicitly; legacy coverage is listed; remaining milestone-C
  obligations stay visible. Supplied C bodies, edits and debug locations survive
  instrumentation; there is no automatic C-to-lift fallback.
- [ ] Design/report changes describe the implemented subset, not completed C.

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

- Not started. Schema and implementation order prescribed; no new acceptance run.

## Findings

- Disc-write notifications carry ranges, not a universal activation identity.
  The schema supplies identity from software operations and input facts.
- Reusing the address table is an implementation step, not a commitment to retain
  unsafe masked callable namespaces; C implements the final namespace contract.
