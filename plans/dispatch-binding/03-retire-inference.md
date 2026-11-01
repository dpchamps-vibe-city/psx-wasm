# C — Complete coverage and retire resident-byte inference

**Status:** not started. **Depends on:** [A](01-explicit-activation.md) and
[B](02-native-references.md). All behavioural policies are specified in the
[plan README](README.md); this milestone implements them rather than reopening
approval or support decisions.

## Deliverable

Support explicit bindings across partial loads, copied/unpacked code, runtime
callbacks, active/suspended execution, hooks and data patches. Enforce the final
callable namespace and migrate supported production inputs to no-inference builds.
Guest-address representation remains valid where it uses explicit bindings.

## Required disposition of current findings

| Finding | Required result |
|---|---|
| D02 identical text/different jump-table targets | Preserve runtime table choice; discover/compile the known target union; no `bad_jump` from an image-specific frozen set |
| D03 main overlap, boundaries and unique entries | Every route uses published availability, including formerly exempt main and unique addresses |
| D04 long → short | Preserve untouched tails; their calls into overwritten regions follow the new bindings |
| D04 complete chunks | Completed reachable code runs before the entire image arrives |
| D04 `chunked-mixed` (lifted fixture) | Execute the known B prefix and A suffix; result **0xB0A** with its corresponding ordered effect. Supplied C is never replaced with lifted fragments to satisfy this test |
| D04 `patched` | The unregistered write invalidates the affected code; reject entry/transfer into unknown instructions, never run stale A |
| D05 arrival mechanisms | Host/native load, guest stores, copy and unpack use the same explicit range-publication semantics |
| D06/D08 transfer and ABI routes | Preserve calls, tails, interior entries, arguments, returns and caller-visible state |
| D07 aliases/namespaces | KUSEG/KSEG0/KSEG1 RAM aliases work; misalignment, unregistered wraparound, unmapped values and stand-in collisions reject under the callable ABI |
| D09 retained suspension | Resume the retained continuation and observe updated independent callees |
| D09 replaced suspension (existing lifted fixture) | Resume at the current version's continuation; **0x22A**, not **0x12A**, without repeating entry effects |
| C native-frame control | Resume the same authored C body after reload; subsequent slot calls observe the new binding; source edits remain observable |
| C explicit-continuation control | Source calls the typed continuation helper; select the current authored C handler and produce the matching **0x22A** transition without lifting either C body |
| D10 ambiguity/absence | Resolve by explicit publication; an absent helper/entry rejects at its attempted transfer, never by candidate ordering |

Do not delete old reports. Remove corresponding `review_required` markers when
installing these specified contracts and their tests; do not mark an observation
passing just by changing its classification. Original-execution expectations
still require independent evidence. Namespace/unknown-code rejection is a port
contract, not an emulated console exception.

## Source seams

- A/B input, planner, lift and linker code.
- `crates/psx-runtime/src/{driver,wasm}.rs`, `crates/psx-bios/src/` and
  `crates/sdk-psyq/src/`: real invocation, queued callbacks and software load events.
- `crates/psxw-link/src/{game,mods,module}.rs`,
  `crates/psxw-modkit/src/build.rs`, `crates/psx-mod/` and their host consumers:
  hooks, synthetic entries, metadata and data patches.
- `crates/psxw-build/src/game.rs`: runtime merge, optimization and build evidence.
- Existing dispatch fixtures/report and `crates/psxw-cli/src/ci.rs`: acceptance.

## C continuation interface to implement

Add `[[dispatch.c_continuations]]` records with these fields:

| Field | Required meaning |
|---|---|
| `slot` | Logical slot, a valid C identifier used in `psxw_continue_<slot>` |
| `placement` | Placement whose publication makes this handler eligible |
| `anchor` | Original continuation address within that placement's text; all registrations for a slot share the same canonical anchor |
| `function` | Authored C handler symbol, unique in the placement's container |
| `state_header` | Input header defining the shared state type, resolved through the existing C include paths |
| `state_type` | Complete C state type; handler signature is `uint64_t function(state_type *)` |
| `bridge` | Optional authored C state-packing function for a lifted-to-C interior transfer |
| `bridge_inputs` | When a bridge is present, ordered lifted values passed as u32 parameters: `r0`…`r31`, `hi`, `lo`, or aligned `stack:<byte-offset>` relative to the continuation's guest SP |
| `bridge_outputs` | Map from lifted register/HI/LO/stack locations to named u32 state fields to copy back after the handler; default empty. `r0`, `r2` and `r3` are not output-map targets |

Generate `psxw_binding.h` with the state-header includes and typed continuation
helper declarations. Obtain handler/bridge definitions from C objects; they need
not have standalone original function addresses. Reject ambiguous/missing symbols
rather than selecting a corresponding MIPS translation.

Fingerprint the canonical state type, field offsets/types, size, alignment and
pointer representations after B's flow classification. All handlers for a slot
must agree. Publication binds that slot according to the anchor's current explicit
provenance. An unavailable anchor has no handler; a call reports the precise slot
and placement instead of falling back to another module or to MIPS.

The C caller creates/owns the state object and explicitly invokes the helper at
the desired transition. Ordinary C and Asyncify rules govern that object's
lifetime. The helper makes an ordinary call into the selected C handler; it does
not rewrite native frames. Authored source controls further transitions, returns
and loops. Do not inject a transition merely because an address was overwritten.

For a reachable lifted-to-C interior transfer, require `bridge`. Its signature
is `void bridge(state_type *state, uint32_t value0, ...)`, with exactly the listed
inputs. Generate an ordinary C wrapper that owns the state object, calls this
source-authored packer, then calls the selected authored handler. Keep the selected
registration fixed for that invocation. Capture inputs before temporary wrapper
stack changes; include them in lifted liveness. Unavailable values are build
errors, never fabricated.

After the handler returns, restore declared `bridge_outputs` from their typed u32
fields and place the u64 result in `v0/v1`; other frame values stay unchanged and
`r0` stays zero. Validate the field/target mappings, including guest stack addressing.
This is explicit state adaptation, not reconstruction of optimized C locals.
Normal o32 calls to a C function's entry do not need a bridge.

Missing required handlers/bridges or incompatible state schemas fail with
`binding.missing_c_continuation` / `binding.c_state_mismatch`, naming the function,
slot and required declaration. A potential reload while an ordinary C frame is
active is not itself an error and does not require these registrations.

## Tasks, in order

### C1. Complete writer coverage and range publication

- [ ] Inventory reachable executable writers, including staging/copy/unpack paths,
  native read completions and existing device-fallback writes. Use conservative
  analysis plus the declared summaries from A. Strict builds report every uncovered
  writer; do not infer completeness from a successful replay.
- [ ] Implement all schema events/effects: instruction-address `after-write`,
  function enter/return, registered native software boundaries, and keyed
  `begin`/`publish`/`end` requests. Unknown fields/effects never silently no-op.
- [ ] Carry private request/instance identity through the existing asynchronous
  I/O job until its actual write/completion boundary. Hosts return bytes/job
  identity, not their own module choice. Reuse the backend's request lifecycle.
- [ ] Instrument lifted writes at original sites and C writes at verified source
  sites, complete-operation boundaries or A's explicit `c_marker` calls. Preserve
  destination, length and source offset. Missing required C correspondence produces
  `binding.missing_c_write_site` with the marker/declaration to supply; it never
  selects the function's lifted implementation.
- [ ] Apply invalidation/publication after the relevant completed write and before
  the next guest observation. Success-only return instrumentation is retained only
  where A's quiescence/failure-before-write condition is established.
- [ ] Handle partial failure by preserving completed published ranges and leaving
  overwritten unknown ranges invalid. Do not restore old identities or invalidate
  untouched tails. Repeated publication with the same provenance is harmless.
- [ ] Track synchronous request contexts per guest thread and nest them correctly;
  keyed requests persist to their declared end. Context survives suspension with
  the existing thread state. No wall-clock or host-specific binding decisions.
- [ ] Use module/placement facts for every supported input. Do not add filename,
  game-ID or loader-instruction special cases to compiler code.

### C2. Compile mixed ranges and dynamic dependencies

- [ ] Partition affected lifted graphs at declared replacement boundaries, callable
  entries and continuation points. Build compiled fragments for the known code at
  those locations. Instruction-aligned mixtures of known fragments are supported;
  incomplete/unknown instructions have no executable binding.
- [ ] Bind lifted transfers between independently replaceable fragments dynamically.
  Keep internal control flow structured where ownership is stable. Do not execute
  one lifted image's suffix after another image replaces that range.
- [ ] Preserve supplied C bodies. A requested C composition/interior transition
  uses the authored handler/state interface above; absent required mappings are
  explicit input errors. Do not split C by original instruction offsets, lift it,
  or impose lifted-frame lifetime semantics on ordinary native C invocations.
- [ ] Preserve the live state required by cross-fragment transfers, including
  existing non-o32 register-sharing edges. Use C3's frame mechanism rather than
  incorrectly forcing every transfer through four o32 arguments.
- [ ] Replace image-specific frozen jump-target assumptions with the union of
  supported discovered/declaratively supplied targets and runtime table reads.
  A runtime target selects its current binding. Missing known-target compilation
  is an implementation failure, not a new unsupported classification.
- [ ] Do not inline across independently replaceable fragments. Permit sharing
  only for equivalent code with equivalent relevant assumptions; candidate names
  and iteration order never affect results.
- [ ] Keep entry/transfer diagnostics specific: requested entry, actual transfer
  location, affected range and unknown/unavailable cause. Never accept an arbitrary
  trap as proof of the intended rejection.

### C3. Preserve source-owned C and implement explicit transitions

- [ ] Enforce tier provenance: supplied C stays C unless the input explicitly selects
  lifting. Keep active C bodies and locals bound to their existing invocation;
  Asyncify resumes them unchanged. Future slot-based calls resolve current bindings.
- [ ] Parse/validate the C continuation registrations above and generate their typed
  helpers, state-ABI checks and explicit binding entries. Compile actual authored
  handlers/bridges; do not obtain their bodies from original instruction bytes.
- [ ] Keep transitions source-driven: only an explicit continuation call or declared
  lifted-to-C interior bridge invokes this interface. Do not generate hidden C
  program-counter switches at arbitrary source lines or require annotations for
  ordinary native frame retention.
- [ ] For functions already selected for lifting, generate original-address
  continuations at relevant calls/safepoints/fragment boundaries, with the union
  of required live values and explicit bridge inputs. Preserve guest stack layout.
- [ ] Resolve current bindings at lifted continuation transfers and restore their
  required live state. Use the frame-local compiled-handle trampoline to avoid
  recursion growth; never rerun a replacement prologue or completed effects.
  A target implemented in C needs its declared C bridge, not a hidden lifted clone.
- [ ] Integrate lifted continuation state with existing Asyncify/thread state while
  leaving ordinary C frame restoration intact. Keep the existing scheduler and
  callback order; this is not a global PC/opcode interpreter.
- [ ] Preserve the existing lifted `D09/suspend-replaced` 0x22A positive and the
  independent-region control. Add separate ordinary-C and explicit-C-continuation
  tests under their respective contracts; never relabel the lifted case to accept
  its old 0x12A result.

### C4. Implement the callable namespace and real boundaries

- [ ] Replace unchecked guest-call masking with the README's canonicalization:
  alignment/null check; exact registered native/synthetic namespaces; permitted
  RAM aliases; then available-entry lookup. No data-memory-map changes.
- [ ] Keep ordinary RAM and stand-ins in separate logical domains even if their
  old masked indexes collide. Use separate lookup entries/tables; do not reserve
  unrelated RAM slots merely because a ROM-shaped pointer shares their low bits.
- [ ] Apply the same policy to direct dynamic calls, guest pointers, IRQ/nested
  callbacks and `guest_invoke`. Fixed native calls retain B's proven path.
- [ ] Keep SDK/BIOS callback fields and public/mod pointer interfaces guest-valued.
  Stored pointers continue following changing slots. Never pass B's native index
  into those interfaces or snapshot a callback's current module at registration.
- [ ] Cover adaptor entries at replaceable addresses through the same bindings.
  Do not change chip policy or reintroduce register-level drivers as a workaround.
- [ ] Version changed runtime/callable ABI artifacts and reject mismatches clearly.
  Private native-table numbering remains link-local.

### C5. Migrate hooks and overlay data patches

- [ ] Point bindings and native generic entries through the appropriate hook heads;
  retain caller-scoped direct hooks, raw/next semantics and existing mod order.
- [ ] Replace `psxmod.resident` and all two-word mod/data-patch selectors with
  versioned `psxmod.bindings` metadata. Records name the base build identity,
  placement/container, affected ranges and binding/load-instance availability.
  Dynamic consumers resolve those identities against the matching base build;
  do not treat a different build's dense IDs as interchangeable.
- [ ] Replace both emitters and consumers in linker, build, modkit and runtime/host
  loading paths. Reject old incompatible records; do not reinterpret their words
  or retain a hidden fingerprint fallback.
- [ ] Apply an overlay data patch only when its full destination range has arrived
  for the intended placement/load instance. Patch it before guest callbacks can
  observe the completed range. A publication of code alone does not assert that
  all data has arrived.
- [ ] Retain exact-range load notifications for this ordering/coverage purpose.
  Ordinary game data writes do not start a new load instance or repeatedly reapply
  a patch. New actual loads do. Neither disc completion nor module name alone is
  used to guess residency.

### C6. Complete conformance and enforce strict builds

- [ ] Reuse D01–D10 under the fixed dispositions above. Add original-MIPS evidence
  for mixed-range execution and retain existing positive observations. Complete
  equivalent-symbol-partition coverage and the bounded sensitivity work already
  specified by the conformance plan.
- [ ] Add `D09/binding-runtime-*` through the real production runtime merge and
  relevant adaptor selection. Exercise a real callback/nested invocation and
  suspension. Fixture imports pretending to be the runtime are insufficient.
- [ ] Include an edited C function with an observable effect absent from its original
  MIPS; suspend it, reload the slot and verify its current frame still executes that
  C, while a fresh slot call enters the newly bound implementation. Check source/tier
  provenance, not only results. Keep this authored-C oracle distinct from the
  existing original-MIPS fixture.
- [ ] Include an authored state/handler continuation that matches the 0x22A original
  transition without a C-to-lift switch. Reject a missing bridge or incompatible
  state type with the specified build diagnostic, not a substituted implementation.
- [ ] Preserve source debug information through instrumentation and Asyncify. Reuse
  existing DWARF checks and the browser-debug flow for a bounded breakpoint check
  in the authored C body and its C continuation, with the explicit state visible.
  Generated helpers are artificial; authored functions/lines must remain real
  breakpoint targets. Do not create a separate debugger framework.
- [ ] Add focused hook/data-patch coverage and a replaceable adaptor entry, reusing
  current mod tests. Native/mod-only behaviour uses explicit ABI assertions;
  original guest code still uses independent reference evidence.
- [ ] Verify both linked and optimized output. Retain selected name/order changes
  and A/B mutations; demonstrate that deliberately reintroducing one legacy
  selector is detected. Do not build a combinatorial variant campaign.
- [ ] Implement `[dispatch].require_explicit = true`: fail the build if any
  execution/mod-selection path retains byte inference, any reachable executable
  writer lacks coverage, or any declared binding rule is unimplemented. List
  offending sites and exact missing facts; a planner's green counter alone is
  insufficient—inspect emitted/merged mechanisms too.
- [ ] Convert all supported production inputs using declarative rules and enable
  that requirement. Keep explicit legacy mode only during migration, then remove
  legacy execution/mod selector generation and its obsolete metadata. Final input
  schema version 2 defaults to explicit binding, rejects `require_explicit = false`,
  and rejects version-1 inputs with a migration diagnostic. Update supported
  manifests to version 2. Never fall back when a rule is invalid or incomplete.
- [ ] Install the full `dispatch` gate in `psxw ci <game>`. It is mandatory and not
  manifest-skippable. Missing reference/tool prerequisites, mismatches and missing
  required coverage produce nonzero exit and fresh non-passing reports.
- [ ] Run both production games' builds, configured semantic regressions and normal
  CI, including the save-backed dispatch regression with its actual card input.
  Missing saves/facts are explicit failed acceptance prerequisites to supply—not
  a policy question, passing skip or justification for a game-specific compiler fix.
- [ ] Update design, ABI/report versions, conformance status, shortcuts/open items
  and the plans index to reflect actual completion. Remove obsolete approval
  placeholders for the contracts this plan implements; preserve historical evidence.

## Exit criteria

- [ ] Required lifted positives, including mixed known ranges and active replacement,
  still match their observations. C native-frame and explicit-transition cases
  match their authored-source contracts; no baseline positive is relabelled away.
  Negatives fail for their defined cause before incompatible execution.
- [ ] Supplied C edits execute, no automatic C-to-lift selection exists, and source
  breakpoints in the C body/handler survive the production debug pipeline. Missing
  C mappings diagnose the exact source interface to supply.
- [ ] Production runtime, callbacks, adaptors, hooks and data patches are covered.
- [ ] Supported production builds require explicit binding and contain no resident-
  byte execution/mod-policy selectors or silent legacy fallback.
- [ ] All declared rules and reachable code-changing operations are accounted for.
  Guest pointers backed by explicit bindings remain permitted.
- [ ] Full mandatory conformance and game acceptance pass with actual prerequisites.
  A missing implementation or input keeps C incomplete; expectations are not weakened.
- [ ] Design, reports and milestone statuses agree with evidence. No universal JIT,
  CPU emulation, or elimination of every runtime helper is claimed.

Commands after implementing the named integration group and migrating inputs:

```sh
cargo run -p psxw-cli -- conform dispatch --case D09/binding-runtime
cargo test -p psxw-build --test dispatch
cargo run -p psxw-cli -- conform dispatch
cargo run -p psxw-cli -- build btn
cargo run -p psxw-cli -- build bof4
cargo run -p psxw-cli -- ci btn
cargo run -p psxw-cli -- ci bof4
```

Enable `dispatch.require_explicit` in both migrated inputs. Run their existing
semantic replay commands with configured saves/assertions; do not invent expected
spawn facts. Verify the same binding rules under both existing host acceptance
paths. Preserve reports when later runs reuse output paths, and run focused tests
for the ABI/runtime/mod seams changed.

## Progress log

- Not started. All formerly deferred behavioural choices now have prescribed
  outcomes and mechanisms; no implementation or acceptance result is claimed.
- Removed both continuation-driven and write-instrumentation-driven C-to-lift
  fallbacks. Added explicit C site/continuation interfaces and source-debugging tests.

## Findings

- Ordinary C frames are source-owned and retained. Explicit C continuation calls
  carry authored state when a transition is required; only already-lifted code uses
  inferred MIPS live-state transport. Neither mechanism substitutes MIPS for C.
- Original pointer representation can remain in a completed native port. What is
  eliminated is runtime identification from instruction bytes.
- From A: an implementation binds only when one publication covers its whole
  footprint (translated regions in its own container, or its C extent). A
  footprint split across publications stays unbound until C2 composes ranges.
- From A: undeclared writes leave bindings stale, not rejected (the A mutation
  showed stale code running). C1's writer coverage and C6's strict mode close this.
- From A: an operation the translator expands into its callers fails with
  `binding.expanded_operation`; C instruments those callers locally.
  `binding.unimplemented_rule` names every rule A rejects (writes at lifted
  instruction addresses, native-boundary sites, `begin`/`end`, keyed requests,
  publication at entry). Supplied C's write sites already publish through A's
  `c_marker` calls; C1 adds the lifted original-PC path for the same rules. `require_explicit = true` and
  `c_continuations` fail with `binding.unimplemented`.
