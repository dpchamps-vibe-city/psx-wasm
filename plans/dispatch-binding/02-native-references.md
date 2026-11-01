# B — Native references where proven safe

**Status:** not started. **Depends on:** [A](01-explicit-activation.md).
Implement the representation contract in the [plan README](README.md). There is
no representation-approval step.

## Deliverable

Produce native calls and address-taken references through proven flows, while
preserving guest representation where the program exposes addresses or requires
late binding. Do not mistake native promotion coverage for correctness coverage:
a guest pointer backed by A's explicit bindings is already a valid no-inference
execution path.

A reference's origin does not establish its lifetime. The same FunctionId can be
reached by a fixed reference, a changing slot and an opaque guest value. Classify
uses/flows, not just target functions. Native promotion preserves the selected
source implementation: supplied C and its edits remain C. Do not switch tiers to
make reference analysis, instrumentation or continuation transport easier.

## Fixed representation

1. Emit ordinary Wasm direct calls for fixed direct edges, retaining signature
   checks, hooks and the existing C/o32 marshalling rules.
2. Append a private `native_functions` table after the linker's existing tables.
   Its table number is link-local, not a public ABI constant. Index zero is null.
3. Assign one nonzero native handle per eligible function identity, sorted
   deterministically, including compiled-symbol identities for source-only C helpers.
   Table entries use the existing uniform o32 callable form, including the correct
   generic hook entry. Per-signature indirect wrappers marshal to that entry; do
   not assign different pointer identities merely for different C declarations.
4. Native handles never pass through `table_index(guest_address)`. Native call
   sites use the private table explicitly. Invalid/null/out-of-range handles
   produce a native-handle diagnostic before any target effects.
5. Keep guest-address encoding in SDK/BIOS callback storage, `GuestCall.addr`,
   `guest_invoke`, host-call interfaces, mod synthetic pointers and persistent/
   externally visible data. Do not add a parallel native callback ABI in this plan.
6. A closed compiled C/lift flow can pass a native handle through an internal
   parameter, return or identified storage cell only when every use is rewritten
   consistently. No tagged pointers, numeric-range inference or structure widening.

## Eligibility and conversions

A flow is eligible only when all of the following hold:

- The FunctionId and fixed binding are established at every invocation.
- All producers, copies, calls and storage consumers in the flow are accounted for.
- No use observes a numeric guest address, serializes its bytes, performs address
  arithmetic, or compares it with an incompatible/unconverted representation.
- Null/equality observations remain equivalent, including the fact that distinct
  module functions can have equal original guest pointer values.
- The selected implementation's code/dependencies are valid for the invocation;
  native promotion does not bypass a replacement or continuation obligation.

Use existing relocations, prototype facts and lift metadata with bounded use
analysis. When any condition is unproven, retain guest representation for the
whole connected flow and record the reason. Do not launch universal alias analysis
or accept a `force_native` assertion that overrides observable semantics.

Guest-to-native conversion is allowed only at a boundary where the fixed identity
and eligible uses are proven. Native-to-guest conversion uses the original address
only when that value preserves the reference's meaning for the full escaping use.
Otherwise do not promote that flow. A changing slot is never converted by taking
a snapshot of its current occupant. These rules are the conversion algorithm,
not decisions left to each implementation site.

## Source seams

- `crates/psxw-inputs/src/map.rs`: preserve resolved identity/provenance.
- `crates/psxw-cc/src/`: retain needed existing prototype/type facts through C
  compilation; do not introduce another C parser/frontend.
- `crates/psxw-link/src/game.rs`: fixed/native/slot call lowering, entries and
  o32 wrappers; replace the target-wide assumption in `func_addr`.
- `crates/psxw-link/src/{rewrite,layout}.rs`: site-specific instruction and data
  relocation classification.
- `crates/psxw-lift/src/{program,graph,emit}.rs`: proven target flows and native
  internal crossings; computed/untyped targets remain guest represented.
- Existing conformance and linker marshalling tests: end-to-end evidence.

## Tasks, in order

### B1. Carry classification through reference flows

- [ ] Extend A's binding plan with reference-site IDs and connected use-flow
  classifications. Include target identity, stability evidence, consumers and
  native/guest crossing information. Reuse compiler structures rather than a new IR.
- [ ] Start with known direct calls, local address-taking through compiled helpers
  and labelled pointer initializers with enumerable consumers. Apply the eligibility
  algorithm above; do not promote unknown original numeric words.
- [ ] Record a deterministic reason for retained guest flows: `late_binding`,
  `numeric_observation`, `opaque_escape`, `missing_uses` or `unstable_dependency`.
  Retention is not an error when explicit address binding preserves behaviour.
- [ ] Extend relocation/resolver interfaces with site context. A symbol appearing
  in both a native and guest flow must receive different lowering at those sites.
  Changing `func_addr()` globally to return an index is not sufficient.

### B2. Emit native calls, handles and eligible data

- [ ] Generate the private table and o32 entries described above. Keep generic
  hook-chain entry semantics; a known function identity does not authorize bypassing
  a mod's hook. Fixed direct calls retain caller-scoped hook handling.
- [ ] Emit native direct/indirect calls and their existing-style marshalling
  wrappers. Do not change the argument/return ABI to solve identity transport.
- [ ] Rewrite instruction relocations and data initializers consistently. Change
  only identified function-reference cells in eligible flows. Preserve the addresses
  and layouts of ordinary guest objects and all unrelated numeric values.
- [ ] Carry native representation through eligible C/lift parameters and returns.
  Rewrite the consuming indirect call explicitly. If lifted arithmetic or another
  use observes the number, retain the entire flow as guest rather than smuggling
  a handle through an ordinary MIPS-address call.
- [ ] Leave opaque callback/runtime/host/mod interfaces unchanged. Test that a
  converted local reference cannot leak a table index into those guest fields.
- [ ] Keep activation instrumentation intact through C optimization, Wasm rewrite
  and production optimization. Native pointer lowering must not erase A's events,
  C source locations or explicit source continuation calls. Treat a continuation
  helper as a changing slot, not a permanent reference to its current C handler.

### B3. Integrate evidence and validate

- [ ] Add native-reference sites, retained-flow reasons and conversion boundaries
  to the versioned build/conformance reports. Distinguish guest explicit bindings
  from legacy byte inference; only the latter is unfinished identity migration.
- [ ] Add a bounded `D06/native-reference-*` group through production preparation:
  fixed address-taking, an eligible relocated pointer table, a mixed C/lift flow,
  and a changing-slot control. Combine assertions rather than multiplying variants.
- [ ] Include a numeric comparison/copy or opaque callback escape that prevents
  unsafe promotion. Verify its original pointer behaviour and that no native handle
  reaches the unchanged callback representation.
- [ ] Assert emitted native table/call usage as well as observable results. A fully
  legacy dispatcher must not accidentally satisfy B's structural acceptance.
- [ ] Compare matching-C/lifted fixtures in linked and optimized output with the
  live reference. Keep edited-C expectations source-authored as in A. Use explicit
  port-contract checks for handle representation, not console addresses.
- [ ] Retain A's unchanged-pointer A → B and edited-C controls. Assert eligible
  native references enter the actual C definition, not a lifted duplicate. Mutate
  one native relocation in the scratchpad and demonstrate a targeted failure;
  restore it before acceptance.

## Exit criteria

- [ ] Fixed direct, address-taken and relocated-data flows use the native mechanism
  through normal compilation, with structural and behavioural evidence.
- [ ] A mixed-tier internal flow works with unchanged argument/return transport.
  An opaque callback/control flow keeps guest values and correct late binding.
- [ ] Null/equality/numeric observations and existing hooks remain correct.
  Unproven flows are retained and explained, never guessed native.
- [ ] A still passes, including preservation of edited C and source locations;
  full-corpus failures remain visible for milestone C. No claim of universally
  native pointer values is made or required.

Commands once the group is implemented:

```sh
cargo test -p psxw-build --lib
cargo test -p psxw-difftest --test link_passes
PSXW_DISPATCH_CASE=D06/native-reference cargo test -p psxw-build --test dispatch
cargo run -p psxw-cli -- conform dispatch --case D06/native-reference
cargo run -p psxw-cli -- conform dispatch --case D05/explicit-binding
cargo run -p psxw-cli -- conform dispatch
```

Run focused tests in changed compiler crates. Keep subset/full reports separately;
required CLI/reference evidence cannot be replaced by tool-skipping unit tests.

## Progress log

- Not started. Native table, eligibility and conversion algorithms are prescribed;
  no ABI or code-generation change has been implemented by writing the plan.

## Findings

- Native handle encoding is useful only where observable guest semantics permit it.
  Keeping an opaque guest value is compatible with complete retirement of inference.
- Ordinary C frames retain their source implementation. Milestone C supplies
  explicit source-owned C transitions and rebinding for already-lifted code;
  native pointer promotion never authorizes automatic C-to-lift replacement.
- From A: the linker counts each reference where it lowers it
  (`psxw_link::game::Ctx::reference`: `fixed`, `slot`, `lookup`, legacy
  classes) and `build.json` reports the totals. Extend that classification for
  B's flows rather than adding a second classifier. Covered addresses' table
  entries are A's bindings, so a native handle for a slot must still read the
  current binding.
