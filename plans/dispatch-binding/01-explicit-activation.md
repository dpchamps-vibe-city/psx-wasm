# A — Explicit activation with the existing pointer ABI

**Status:** not started. **Depends on:** Gate A in the [plan README](README.md).
Read that README and the [conformance plan](../dispatch-conformance.md) first.

## Goal and context

Deliver an end-to-end explicit-binding path through the production compiler:

```text
declarative operation facts -> compiler instrumentation -> activate module
                                                           |
                                                           v
same original pointer P -----------------------------> current entry[P]
                                                           |
                                                           v
                                                    compiled function
```

Retain D6 guest pointer values, its existing calling boundaries and the guest
memory layout. Replace sampled overlay selection only for the explicitly covered
regions. Keep other regions visibly legacy during this milestone.

The foundation exists: `GameSpec.tiers` identifies `(container, address)` entries;
`Resolved` retains some symbol provenance; the lift tier distinguishes static
and dynamic locations; the linker already generates wrappers and dispatch tables.
There is no established activation contract. Existing disc-write notifications
used by mods are not such a contract.

The initial vertical slice uses resident loading code and known placements with
no observable execution in a changing region before its declared publication
point, and no active/suspended translation depending on that region. Demonstrate
these facts for the fixtures. This bounds the first implementation; it does not
change the support classification of existing partial-load/re-entry fixtures.

## Files and responsibility

| Area | Primary seams |
|---|---|
| Input facts and resolution | `crates/psxw-inputs/src/{game,map}.rs` |
| Binding plan and production preparation | `crates/psxw-build/src/game.rs` |
| Call classification and embedded dependencies | `crates/psxw-lift/src/{program,graph,emit}.rs` |
| Wrappers, activation functions, table entries | `crates/psxw-link/src/{game,rewrite}.rs` |
| Shared ABI, only if a boundary changes | `crates/psxw-abi/src/lib.rs` |
| Fixture input and production integration | `crates/psxw-build/src/dispatch_conformance/{fixture,pipeline,corpus,mod}.rs` |

Prefer crate-private helpers in these owners. Do not create a new crate or
reimplement `prepare`, linking or optimization in the fixture runner.

## Tasks, in order

### A1. Inventory references and record the initial contract

- [ ] Inventory direct C calls, C address relocations, lifted direct/indirect/tail
  transfers, original pointer tables, runtime callbacks, stand-ins and mod hooks.
  Record current bypasses: shared-address-only planning, main-container exemptions
  and uniquely addressed functions in replaceable regions.
- [ ] Define the conceptual categories `Function(module, entry)`,
  `Slot(region, entry)` and unresolved `GuestAddress(value)`. Add only the metadata
  needed to express these alongside current structures. A still encodes callable
  pointers as guest values; category names do not authorize native handles yet.
- [ ] Record Gate A's exact supported placements, publication event, initial state,
  replacement extent and failure behaviour in the design. Get approval before
  replacing D7 selection. Inventory, evidence and ordinary fixes need not wait.
- [ ] Describe what makes an initial fixture's activation quiescent. Do not claim
  arbitrary loader completion is atomic, or that a suspended frame disappears
  when its entry-table slot is changed.

### A2. Add a small declarative activation contract

- [ ] Extend versioned input facts with named executable regions/placements and
  activation rules. Derive original code ranges and function entries from existing
  container metadata where possible; do not duplicate splat's function inventory.
- [ ] Each rule must identify: the operation by container-qualified symbol or
  entry address; the event boundary; the module selector; the compiled destination
  placement; and the condition under which bindings are published.
- [ ] Start with a bounded vocabulary: a literal module or a finite mapping from
  an operation argument to module IDs; a declared placement or validated destination
  argument; successful return from a known operation. Use explicit return predicates,
  not embedded code or an expression-language/plugin framework.
- [ ] A completion callback can be the operation when the necessary facts are
  available there. Never annotate an asynchronous request's return as completion
  merely because it is easier. Facts unavailable at the chosen boundary remain
  unresolved, with their required dataflow identified for C.
- [ ] Validate named modules, entries, selector mappings, original placements,
  region conflicts and rule provenance before linking. A destination requiring
  relocatable code is not implemented by changing a guest address constant; report
  that missing capability rather than pretending a new placement is supported.
- [ ] Retain legacy behaviour for undeclared regions and report it. An invalid or
  incomplete rule for a declared explicit region is an error, not permission to
  fall back to fingerprints. Keep ordinary input errors distinct from original
  program execution rejections.

The proposed vocabulary is an implementation starting point, not approved TOML
syntax. Specify and review its schema with Gate A, then use that one schema for
real inputs and synthetic fixtures. Do not hide activation facts inside tests.

### A3. Produce one binding plan for all call routes

- [ ] In production preparation, combine activation facts, code ranges and tiers
  into a deterministic binding plan. Keep module identity, compiled placement and
  reference classification separate from the final Wasm function index.
- [ ] Enumerate every entry affected by a declared replacement region, not only
  addresses shared by multiple containers. Include main-image overlaps and unique
  entries. Preserve unrelated regions and existing stand-in namespace protections.
- [ ] Record initial bindings and the precise changes for each activation. Shorter
  replacements must follow the reviewed extent contract: neither preserve every
  old entry nor invalidate an entire container by assumption. Stale untouched bytes
  and explicitly unavailable bindings are different facts.
- [ ] Feed C direct calls, lifted direct/tail calls and indirect dispatch from this
  same plan. A call into replaceable code must not bypass it merely because its
  symbol came from the current or main container.
- [ ] Retain established direct calls outside affected regions. Within affected
  flows require an actual stability argument; otherwise lower through a slot.

### A4. Generate activation and instrument real operations

- [ ] Generate module-specific binding updates and a small activation entry in the
  linked guest module, reusing its dispatch table and o32 entries. No separate
  runtime crate, host-controlled module selector or generic dynamic linker is needed.
- [ ] Initialize bindings during the existing boot path before calls can enter
  affected regions. Use existing unavailable-entry diagnostics where the approved
  contract permits them; specify the cause rather than accepting any Wasm trap.
- [ ] Instrument the declared operation boundary in both C and lifted code. Save
  selector/destination arguments when needed, preserve returns and calling state,
  and publish only at the declared successful event.
- [ ] Ensure every route to the operation receives the instrumentation, including
  direct C calls and calls via pointers. Insert it before an optimizer can erase
  the boundary, or deliberately preserve that boundary. Do not disable optimization
  globally or rely on an incidental surviving symbol name.
- [ ] Publish updates without a guest callback/safepoint observing half of one
  logical activation. Do not add a scheduler redesign. An ordinary load with
  observable intermediate availability needs the finer contract in C, not a
  fictitious whole-image atomic commit.
- [ ] Validate failure handling. A failure before writes can leave old bindings;
  a failure after destructive writes cannot automatically do so. Reject an
  insufficient activation description rather than hiding this distinction.
- [ ] Leave load/copy/unpack side effects in the compiled program. Activation
  updates selection state; it is not a replacement for the original data operation.

### A5. Guard translation assumptions and document compatibility

- [ ] Audit discovered jump-table targets and expanded callees in the selected
  flows. Prevent a static call or expansion across a changing dependency unless
  the necessary stability is established. Preserve ordinary runtime table reads.
- [ ] If current lifting cannot preserve a necessary dependency, report the exact
  gap and keep acceptance blocked for that flow. Do not compensate with a larger
  code fingerprint or declare the original positive case unsupported.
- [ ] Keep C/lift arguments, SDK/BIOS callbacks and `guest_invoke(addr, ...)` using
  existing guest-pointer semantics in A. Verify that these routes can reach the
  newly updated table without inventing a second binding authority.
- [ ] Add versioned binding-plan evidence to the build/conformance reports:
  activation rules, initial bindings, slot references, fixed calls and every
  remaining legacy region. List runtime/mod consumers still outside A's coverage.

### A6. Validate the vertical slice through production

- [ ] Add a small `D05/explicit-binding-*` fixture group to the existing corpus.
  The original MIPS loading routine must actually copy/load the module. Supply
  activation metadata through A2 and let the compiler emit the event; the host
  must not call an activation API between fixture checkpoints.
- [ ] Cover A → B → A, an unchanged saved pointer, direct and indirect callers,
  and a second independent region whose bindings must remain unchanged. Use a
  lifted variant and a mixed C/lift variant, not a Cartesian product of every tier.
- [ ] Include a failure-before-write control and input-validation checks for bad
  module/placement facts. Keep port-contract rejection checks separate from the
  original-MIPS observations.
- [ ] Reuse applicable D02 data/dependency controls and D03 route/overlap evidence.
  Record which now use explicit bindings and which are still legacy; a legacy
  pass does not demonstrate the new path.
- [ ] Compare linked and production-optimized Wasm against the live reference and
  authored observations. Assert the covered paths contain no resident-word selector.
- [ ] In the scratchpad, suppress one activation update and demonstrate that the
  saved-pointer fixture fails. Revert the mutation; record it and its result.
  Do not launch a new large mutation campaign.

## Exit criteria

- [ ] Gate A is recorded; input rules and bindings flow through normal preparation,
  link and optimization, not only a test hook.
- [ ] Selected C/lift cases agree with original execution and distinguish stale
  bindings from correct activation. The original pointer values remain unchanged.
- [ ] Declared explicit paths have no code-word residency reads. Other paths and
  unresolved assumptions are individually reported, not silently classified fixed.
- [ ] No unreviewed namespace, active-code lifetime or rejection policy was added.
- [ ] Required tool/reference runs succeeded without skips; the full corpus's
  remaining failures retain their evidence and classification.

Commands after the new fixture group is implemented (`D05/explicit-binding` is
planned here, not an existing case selection):

```sh
cargo test -p psxw-inputs --lib
cargo test -p psxw-build --lib
cargo test -p psxw-difftest --test link_passes
PSXW_DISPATCH_CASE=D05/explicit-binding cargo test -p psxw-build --test dispatch
cargo run -p psxw-cli -- conform dispatch --case D05/explicit-binding
cargo run -p psxw-cli -- conform dispatch
```

The last command may remain nonzero at A. Preserve its report separately from
subset evidence and compare findings; this is not full conformance completion.
Run additional focused lift/link tests only for the code changed.

## Handoff to B and C

Give B the reference categories, event-preservation rules and conversion-sensitive
reference sites. Give C the exact unsupported instrumentation forms, remaining
legacy consumers, translation assumptions and intermediate/lifetime cases. Do
not broaden A's support claim to those cases without implementation and evidence.

## Progress log

- Not started. Plan authored; no implementation or acceptance evidence claimed.

## Findings

- A can reuse the address-indexed table without changing the numeric guest pointer
  ABI. That is explicit activation, not native-reference migration.
- Existing mod disc-write notifications say which bytes arrived, not which complete
  module became callable. Their presence does not satisfy A2.
