# Dispatch binding: explicit identity instead of resident-byte inference

**Status:** prescriptive implementation plan; execution not started. The operator
requested this plan, not a blanket change to D5/D6/D7 or to the supported
execution contract. Resolve the specific decision gates below before the work
that depends on them. Ordinary fixes within the existing contract may proceed;
do not treat every compiler change as requiring a new architectural decision.

## Read first

- [Project conventions](../../docs/conventions.md) and
  [architecture](../../docs/emulation-layers.md).
- [Design](../../docs/design.md), especially §§1–4 and D5/D6/D7/D11.
- [Problem statement and diagrams](../dispatch-identity-and-binding.md).
- [Dispatch conformance](../dispatch-conformance.md): current evidence, proposed
  contracts, failures and unfinished harness/gate work. This plan supplies a
  remediation path; it does not replace that independent correctness contract.

## Context

The tool compiles separate functions identified by `(container, guest address)`.
C, lifted MIPS and native adaptors already have distinct compiled entries. But
function pointers retain guest addresses. Two modules can use the same address
P for different functions at different times.

Loading original bytes into game memory does not replace a Wasm function. The
current linker uses sampled original code words to choose a precompiled function
at shared addresses. Additional distinguishing words fix some image collisions,
but do not establish translation validity. Identical code can depend on different
runtime tables or replaceable callees; mixed bytes can pass the same probes.

The target architecture has two paths through one binding model:

```text
                            code reference
                                  |
                    +-------------+-------------+
                    v                           v
              proven fixed                 late-bound slot
                    |                           |
             native call/ref           activation sets binding
                    |                           |
                    +-------------+-------------+
                                  v
                           compiled function
```

A third category, unresolved guest-address references, remains visible during
migration. Do not guess that such a reference is fixed or silently label its
legacy behaviour native.

Activation is compiler-support runtime functionality, not CPU emulation. The
compiler/input contract establishes when a module becomes callable; generated
runtime code publishes the bindings. The browser or native host does not decide
which module a guest address means. A generic binding runtime can remain in the
finished port: removing every runtime helper is not the goal.

## Deliverables and order

| Milestone | Result | Depends on |
|---|---|---|
| [A — Explicit activation, existing pointer ABI](01-explicit-activation.md) | Declarative activation facts, common binding plan, generated activation events and updated guest dispatch entries; bounded C/lift integration | Approval of the explicit activation contract |
| [B — Native references where proven safe](02-native-references.md) | Direct/native references through proven flows, relocations and explicit interoperability boundaries; remaining guest references reported | A's binding model; approval of representation/conversion rules |
| [C — Coverage and retirement of inference](03-retire-inference.md) | Runtime/mod integration, reviewed difficult cases, complete supported-path coverage and enforcement of no byte-based execution selection | A/B; reviewed lifetime/overlap policies and conformance completion |

Do these in order. A is a real vertical slice, not merely a table API. B delivers
native references incrementally, not a claim that all original integers have
been converted. C is required before claiming production dispatch no longer
infers code identity from bytes.

### Where the six workstreams land

| Workstream | A | B | C |
|---|---|---|---|
| Common reference/binding model | Inventory, classify and report | Carry per-reference identity through lowering | Close supported-path coverage gaps |
| Fixed-reference path | Retain existing proven direct calls | Native references, relocations and conversion boundaries | Finish relevant consumers |
| Activation runtime | Initial state and generated binding updates | Reuse; never snapshot slots as fixed functions | Replacement/lifetime cases and production rollout |
| Activation facts/instrumentation | Small declarative contract; C and lift instrumentation | Preserve events through native lowering | Cover supported arrival mechanisms |
| Translation assumptions | Prevent unsound binding/expansion in the vertical slice | Proof obligations for native promotion | Jump tables, independent regions, suspension and remaining findings |
| Callable boundaries | Preserve guest ABI; inventory all consumers | Explicit native/guest interoperability | Real runtime, callbacks, stand-ins, hooks and data patches |

## Scope boundaries

Keep these unless a separate reviewed change requires otherwise:

- Guest data-memory layout, scratchpad placement and ordinary data pointers.
- The C and MIPS-to-C frontends, existing o32 marshalling and native adaptors.
- SDK → adaptor → native backend; host integration remains below that boundary.
- Ahead-of-time compilation. No production MIPS interpreter, JIT, console-wide
  memory bus, instruction-count scheduler or register-level SDK detour.
- The existing synthetic/reference harness. Extend its production seams rather
  than create another compiler, oracle framework or matrix of every variant.

Do not add whole-code hashes, watch every store, or invent a universal provenance
engine as a shortcut to completing this plan. Do not patch an individual game's
loader in compiler source. Game facts belong in versioned declarative inputs;
SDK facts belong in `boundary/` where applicable.

A source symbol's container is provenance, not proof of a permanent binding:

```text
activate A; p = P; call p -> A
activate B;        call p -> B
```

A native reference to A would be wrong for this flow. Preserve the changing slot.
Likewise, retaining original data addresses is independent of choosing native
code references; do not turn this into a whole-memory-layout rewrite.

## Shared implementation rules

1. **Reuse the identities and wrappers we have.** Add a small binding plan beside
   existing compiler structures, not a replacement IR or a new runtime crate.
2. **Decide per reference/flow, not merely per function.** The same compiled
   function can be reached through fixed and late-bound references.
3. **Keep unknowns explicit.** Every fallback needs a site/region, reason and
   coverage status. Metadata for a declared explicit region must not silently
   fall back to byte probing if it is incomplete or invalid.
4. **Separate image identity from translation validity.** Audit embedded code,
   discovered jump targets and cross-region calls. Preserve dynamic dependencies
   or establish their stability; do not add mutable data to an image fingerprint.
5. **Keep optimization enabled.** Activation events and reference semantics must
   survive ordinary C compilation, Wasm rewriting and `wasm-opt -O2`.
6. **Do not shrink the correctness contract to pass tests.** A's bounded fixtures
   are an implementation boundary, not a declaration that partial loads or
   active-code replacement in the existing corpus are unsupported.
7. **Make ordering explicit.** Initial publication, re-entry, callbacks and data
   patches must not observe an accidental half-updated binding map.
8. **Version actual boundaries.** Bump changed report/custom-section/runtime ABI
   versions and reject incompatible consumers. Do not introduce an ABI break for
   a helper entirely private to generated game code.

## Decision gates

Record the resolution and supporting evidence in `docs/design.md` and its
 decision log before implementing the corresponding architectural change. Do not
restore the removed D72 or treat the deferred D69 proposal as already approved.

| Gate | Decision required | Work it gates |
|---|---|---|
| A | Activation facts, callable regions/placements, initial/replacement bindings and failure semantics for the initial explicit path; D7 change while retaining D6 values | Publishing explicit bindings instead of byte identification |
| B | Native reference representation, pointer-observable behaviour and conversion boundaries; the relevant D6 change | Emitting native references into eligible flows |
| C | Remaining executable overlap, intermediate availability, active/suspended lifetime and namespace policies | Broad rollout and retirement of legacy execution selection |

For C, classify each outstanding finding as an ordinary implementation defect,
a missing input fact, or a concrete architecture decision. Do not replace this
classification with a blanket approval request. Rejections are not proof of
support for a positive original-MIPS case.

## Evidence and reporting

Use existing `build.json` and `out/conform/dispatch/{report.json,summary.txt}`.
Add deterministic, versioned binding evidence rather than a separate reporting
platform. Record:

- Function/region identity, reference site and binding classification.
- Activation-rule provenance and selected compiled placement.
- Translation assumptions relevant to binding and why native promotion is safe.
- Every byte-inference or unresolved compatibility path, including mod consumers.
- Input/rule hashes, compiler revision, toolchain/reference provenance and whether
  the run is a subset. Counts must not hide the individual remaining sites.

Use original MIPS as the independent oracle for original program behaviour.
Activation annotations belong to the compiler input: the reference program still
performs its real load/copy/unpack and does not receive a host-authored activation
answer. Port-only handle/ABI rules require explicit port-contract tests, not an
invented console oracle.

A's and B's selected acceptance cases can pass while the full corpus remains
red. Preserve those full reports and their classifications. Missing reference
input, an unresolved required contract, a timeout or an arbitrary trap is never
an acceptance pass. Unit tests that skip without tools are not sufficient exit
evidence; run the CLI with its required tools/reference configured.

## Completion and maintenance

- Keep each milestone's tasks, findings and progress log current. Carry findings
  affecting later work into the later milestone's Context section.
- Update this table and `plans/README.md` when status changes. Do not mark the
  whole effort complete when only A or B is done.
- C's exit requires covered execution and mod-patch paths to be free of resident-
  byte inference, supported-case evidence, and the reviewed conformance gate.
  Legitimate diagnostics comparing original bytes need not be deleted.
- Broad architectural closure is blocked if necessary input facts or supported
  lifetime semantics remain unresolved. Say what is missing; do not relabel a
  partial migration complete.
- Preserve unrelated working-tree changes and existing replays. Use the session
  scratchpad for mutations and exploratory work; generated outputs stay ignored.

## Tasks

- [ ] Complete milestone A and record its evidence.
- [ ] Complete milestone B and record its evidence.
- [ ] Complete milestone C and reconcile the conformance plan's remaining work.

## Progress log

- Plan authored from the identity/binding discussion; no implementation or new
  acceptance run is claimed.

## Findings

- Existing byte inference also serves mods/data patches. Function dispatch alone
  is insufficient for the final no-inference claim.
- The present synthetic runner does not merge the real runtime. C must close
  that gap rather than extrapolate from fixture callback stubs.
