# B — Native references where proven safe

**Status:** not started. **Depends on:** [milestone A](01-explicit-activation.md)
and Gate B in the [plan README](README.md).

## Goal and context

Promote proven fixed references to direct calls or native function-table handles,
while retaining explicit slots for changing bindings and guest representations at
boundaries that still require them.

```text
proven fixed call/ref -> native entry
changing reference   -> A's binding slot -> current native entry
unresolved flow      -> reported guest-address compatibility path
```

A establishes activation without changing guest pointer values. B must not
reinterpret its slots as permanent references to the module currently occupying
them. A saved pointer that must follow A → B remains late-bound.

The current `func_addr()` drops symbol provenance and returns a guest address.
It serves code relocations and data initializers. Returning a Wasm index instead,
without changing the relevant consumers, is incorrect. Native promotion is a
per-reference/flow decision, not a global property of a target symbol.

Carry forward A's actual findings before starting. Do not assume every event
boundary, dependency or pointer flow was covered merely because A passed.

## Files and responsibility

- `crates/psxw-inputs/src/map.rs`: retain resolved identity and provenance.
- `crates/psxw-cc/src/`: existing prototype/compilation metadata where additional
  reference information must survive C lowering; do not create a new C frontend.
- `crates/psxw-link/src/game.rs`: `func_addr`, direct/indirect call resolution,
  native entries and o32 wrappers.
- `crates/psxw-link/src/{rewrite,layout}.rs`: instruction/data relocations and
  their reference-site context.
- `crates/psxw-lift/src/{program,graph,emit}.rs`: proven targets and preserved
  dynamic calls. An integer-valued MIPS register is not inherently a native handle.
- `crates/psxw-abi/src/lib.rs`, `crates/psx-runtime/src/{driver,wasm}.rs` and
  `crates/psx-bios/src/machine.rs`: only where an actual callable boundary changes.
- Existing dispatch fixtures and linker marshalling tests: integration evidence.

## Tasks, in order

### B1. Specify representation and observable behaviour

- [ ] Review Gate B and record the native-reference/guest-reference contract in
  the design before changing pointer lowering. Address null, equality, casts,
  pointer arithmetic, copies and persistent/external storage wherever observable.
- [ ] Use existing native Wasm calls for fixed direct edges. For address-taken
  native references, prefer an explicit native-table domain with the appropriate
  signature/wrapper. Keep the guest-address dispatcher distinct. Choose actual
  table/entry encoding after checking runtime merge and mod table ownership.
- [ ] Carry the representation in compiler metadata and typed callable boundaries.
  Never determine whether an arbitrary `u32` is a handle by guessing its numeric
  range or passing it through the guest pointer mask.
- [ ] Preserve original guest representations in opaque/shared flows by default.
  Do not introduce a mixed tagged pointer ABI unless a demonstrated boundary needs
  it and that ABI is separately reviewed. No blanket widening of guest structures.
- [ ] Record an explicit conversion rule for each boundary. A fixed native reference
  may convert to a guest address only if that address preserves the reference's
  meaning throughout the relevant use. A slot cannot convert to a fixed function
  by snapshotting its current occupant.

Two native identities can correspond to the same original numeric pointer.
Giving them different handle values can change observable comparisons. Likewise,
a handle passed through an integer field or serialized byte-for-byte is not made
safe by retaining a 32-bit width. Prove a closed flow, preserve the guest value,
or implement a reviewed semantic conversion; do not dismiss these cases as UB.

### B2. Establish eligibility with bounded analysis

- [ ] Extend A's binding plan with promotion evidence per reference site/flow:
  target identity, binding stability, compatible signature and all relevant
  producers/consumers. Reuse relocation, prototype and lift metadata first.
- [ ] Start with closed flows whose uses can be enumerated: known direct calls,
  local address-taking passed through known compiled helpers, and labelled
  function-pointer initializers with fully understood consumers.
- [ ] Treat unlabelled original words, numeric pointer construction, unknown memory
  aliases and opaque integer escapes conservatively. Retain the guest path and
  explain why. Do not build a whole-program pointer-analysis framework as a
  prerequisite to the first native path.
- [ ] Where metadata is needed, describe types/references and binding facts, not
  desired test results. A declared pointer table alone does not prove that its
  values are never compared or exposed elsewhere.
- [ ] Audit stability across re-entry/activation and translated dependencies.
  Container provenance, current residency or identical source instructions are
  not sufficient. Do not fold mutable target tables or inline changing callees
  merely to make an edge eligible.

### B3. Lower references consistently across code and data

- [ ] Extend resolver/relocation interfaces with the smallest reference-site
  context needed. The same symbol may appear in a native flow and a guest flow;
  a single target-wide `func_addr` replacement cannot express that distinction.
- [ ] Lower proven fixed calls directly, retaining existing signature checks,
  o32 conversions and applicable hook entry semantics. Do not bypass hooks by
  linking to a raw implementation just because its identity is known.
- [ ] Emit native table handles only for eligible address-taken flows. Generate
  native indirect calls using their agreed signature/wrapper rather than routing
  them through address masking or A's late-bound table.
- [ ] Apply the same classification to instruction relocations and data
  initializers. Change only identified function-reference cells with understood
  consumers. Preserve ordinary guest data addresses and object layouts.
- [ ] In lifted code, carry fixed-target information where established. Preserve
  computed address/slot semantics otherwise; do not rewrite every constant equal
  to a code address or assume all MIPS indirect calls are native-eligible.
- [ ] Keep activation instrumentation and native references intact through normal
  C optimization, rewrite and Wasm optimization. A useful native promotion must
  survive the production path, not only an unoptimized test object.

### B4. Implement explicit interoperability boundaries

- [ ] Enumerate crossings for each promoted flow: C/lift parameters and returns,
  stored references, callbacks, runtime invocation, stand-ins and mods. Record
  the representation expected on both sides.
- [ ] Reuse o32 marshalling for argument/return transport; code identity does not
  require rewriting the calling convention. Add a conversion only where it is
  semantically valid and every consumer of that boundary agrees.
- [ ] Keep existing `GuestCall.addr`/`guest_invoke` and SDK callback storage guest-
  represented unless deliberately migrated. Do not insert a native index into
  them. If a native-only callable boundary is required, make it explicitly distinct
  and version it; do not overload the old address argument silently.
- [ ] Preserve late-bound callbacks: registering a reference before activation of
  B must not permanently select A when original semantics require following the
  slot. Where a conversion cannot preserve this, keep the whole flow in the guest
  representation and report the boundary preventing promotion.
- [ ] Keep BIOS/kernel stand-ins and mod synthetic references in their existing
  namespace unless the reviewed contract changes it. Their numeric similarity to
  a proposed handle is not permission to reinterpret them.
- [ ] Bump actual shared ABI/custom-section versions changed by this work, and
  verify incompatible consumers fail clearly. Purely link-private handles need
  not create a new host interface.

### B5. Report coverage and test native flows

- [ ] Extend A's report with native-reference sites, native/guest crossings,
  remaining slot references and rejected promotion reasons. Distinguish preserving
  a valid guest path from failing a declared no-compatibility requirement.
- [ ] Add a bounded `D06/native-reference-*` group using the production planner and
  linker: a fixed address-taken call, an eligible relocated function-pointer table,
  and a changing-slot control. Include a mixed C/lift crossing and a callback flow
  that deliberately retains guest representation. Combine cases where practical;
  do not multiply every fixture by every tier and optimization mode.
- [ ] Add a pointer-observation control: a comparison/copy or opaque escape must
  retain equivalent semantics and prevent an unsafe promotion. Original bytes
  need not equal private native handles; original program observations must.
- [ ] Assert the native cases actually emit/use native references, while the slot
  control still observes A → B through the same saved reference. Behaviour alone
  cannot distinguish an all-legacy implementation from completed B work.
- [ ] Compare the original fixture behaviour against the live reference before and
  after production optimization. Use separate explicit port-contract assertions
  for native handle encoding; do not pretend that encoding is a PSX pointer.
- [ ] Make one isolated mutation that misroutes a native relocation/call and show
  the native case fails. Reuse A's stale-binding control rather than duplicating
  its whole mutation exercise.

## Exit criteria

- [ ] Gate B is recorded and the actual representation follows it.
- [ ] At least one fixed call, address-taken flow and data relocation use the new
  native path through production preparation/link/optimization, with evidence.
- [ ] At least one mixed-tier boundary is exercised without changing the existing
  argument/return ABI, and an opaque callback boundary retains correct guest values.
- [ ] Changing slots are never accidentally frozen. Unsafe integer/opaque escapes
  are reported and remain correctly represented, not guessed native.
- [ ] Native references do not bypass hooks or acquire a dependency on module-name
  ordering. Original data layout and unrelated pointer namespaces remain intact.
- [ ] A's acceptance still passes. The full corpus remains honestly reported;
  unresolved lifetime and broader runtime coverage transfer to C.

Commands after the planned fixture group exists:

```sh
cargo test -p psxw-build --lib
cargo test -p psxw-difftest --test link_passes
PSXW_DISPATCH_CASE=D06/native-reference cargo test -p psxw-build --test dispatch
cargo run -p psxw-cli -- conform dispatch --case D06/native-reference
cargo run -p psxw-cli -- conform dispatch --case D05/explicit-binding
cargo run -p psxw-cli -- conform dispatch
```

Run focused tests in `psxw-cc`, `psxw-link`, `psxw-lift`, `psxw-abi` or runtime
crates when changed. CLI/reference runs must not be replaced by skipping tests.
Preserve subset reports before a full run overwrites the common report paths.

## Handoff to C

List every remaining guest/legacy flow and distinguish these outcomes:

1. A guest reference that resolves through explicit bindings: valid compatibility,
   not a remaining byte-inference dependency.
2. A reference still requiring resident-byte inference: unfinished migration.
3. A flow lacking sufficient facts or correct translation semantics: unresolved
   support, not a passing compatibility result.

C need not turn every case in category 1 into a native handle. It must close
category 2 for supported no-inference builds and resolve category 3 honestly.

## Progress log

- Not started. No pointer encoding or ABI change is implemented by this plan.

## Findings

- Fixed binding and native numeric encoding are separate decisions. A proved
  direct call can be native while an escaping pointer to the same function must
  retain its guest representation.
- Removing guest addresses everywhere is not the exit condition. Removing unsafe
  identity recovery while preserving behaviour is the architectural objective.
