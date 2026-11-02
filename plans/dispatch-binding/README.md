# Dispatch binding: implementation plan

**Status:** not started. **Direction:** native references for proven fixed
bindings; explicit activation for changing bindings; no resident-byte inference
in the completed build. This document specifies the implementation contract.
There are no pending approvals, policy choices or design gates in this plan.

Missing facts produce the diagnostics specified below. Missing mechanisms are
implementation tasks. Neither is an invitation to choose weaker semantics or
ask the operator to design the mechanism. Keep implementation and acceptance
status separate: writing this plan does not claim that production has changed.

## Read first

- [Project conventions](../../docs/conventions.md).
- [Architecture](../../docs/emulation-layers.md): SDK → adaptor → native backend;
  hosts integrate with the backend. This is a static compiler/native port.
- [Current design](../../docs/design.md), particularly D5/D6/D7/D11.
- [Problem statement](../dispatch-identity-and-binding.md).
- [Conformance evidence](../dispatch-conformance.md).

The contracts here replace the earlier dispatch-specific approval placeholders.
Preserve historical reports, but do not carry their `review_required` labels
forward as unresolved decisions where this plan supplies the answer. Update the
design and decision log alongside implementation, describing what actually lands.

## Context

The compiler already identifies functions by `(container, guest address)` and
emits separate Wasm implementations. Original references can nevertheless use
one address P for different functions at different times. Loading MIPS bytes into
game memory does not replace either compiled function.

Today, shared addresses use sampled instruction words to identify a candidate.
That distinguishes some known images, but cannot establish all translation
assumptions. Identical instructions may depend on runtime tables; partial loads
can combine known code; suspended execution can resume into replaced code.

The replacement is:

```text
                           code reference
                                 |
                   +-------------+-------------+
                   v                           v
             proven fixed                 changing slot
                   |                           |
             native call/ref          explicit binding lookup
                   |                           ^
                   |                  publication updates it
                   +-------------+-------------+
                                 v
                         compiled implementation
```

Guest-address references remain at opaque boundaries, but their lookup uses
explicit bindings, not instruction bytes. A generic binding runtime is legitimate
compiler support. Eliminating all runtime support or relocating all data is not
the objective.

## Milestones

| Milestone | Deliverable | Dependency |
|---|---|---|
| [A — Explicit activation](01-explicit-activation.md) | Input facts, binding plan, real operation instrumentation and generated binding updates; original pointer ABI retained | None |
| [B — Native references](02-native-references.md) | Native calls/handles in proven closed flows; explicit preservation of guest representation elsewhere | A |
| [C — Coverage and retirement](03-retire-inference.md) | Partial replacement, source-owned C continuations and lifted continuation rebinding, runtime/mod integration and no-inference builds | A and B |

A and B are bounded implementation increments. Their passing subsets do not
complete C or erase the remaining full-corpus failures.

## Contract 1: identities, placements and data

- Guest-associated functions retain `(container, original entry)` identities.
  Source-only C helpers and continuation handlers use qualified compiled-symbol
  identities `(container, translation unit, symbol)` instead; do not invent original
  PSX addresses for them. Dense runtime IDs are assigned deterministically at link
  time; numeric IDs are not part of the guest ABI.
- A placement is a container's compiled original address layout, derived from
  its segments. Compile additional placements explicitly; do not pretend an
  arbitrary destination relocates already-compiled code.
- A slot identifies a callable entry or continuation at a placement address.
  Overlapping containers share the affected slots; unrelated regions do not.
- Supplied game C is authoritative, including edits that differ from the original
  binary. Use lifting only for missing C implementations or an explicit per-function
  lift selection. Dispatch, instrumentation and continuation limitations must never
  trigger an automatic C-to-lift fallback. Existing SDK/adaptor policy is unchanged.
- Keep D5's data-memory window, scratchpad, structure layouts and data pointers.
  Keep o32 transport at existing C/lift/runtime boundaries.
- Preserve function identity separately from binding semantics. `p = P` can name
  a changing slot even when P initially belongs to a known module. Never freeze
  such a reference to its initial occupant.

## Contract 2: executable availability and activation

Maintain a compiler-private interval map of published executable provenance:
`(destination range, module, source range, load instance)`, plus generation
counters for affected bindings. Key ranges by D5 backing-window offsets so writes
through data-address aliases affect the same availability; retain original guest
addresses for diagnostics. This is event-derived state, not a scan of RAM.

1. Boot publishes the executable ranges actually supplied by the boot image.
   Merely having compiled a module does not make it callable.
2. A declared code write invalidates exactly the bytes it replaces. A subsequent
   publication records the known source identity of the completed, visible range.
3. Publication is range-based, not an atomic whole-overlay transaction. Untouched
   tails retain their previous provenance. Independently replaced ranges retain
   independent bindings.
4. A completed copy/unpack publishes its established source identity. Unknown
   writes invalidate without publishing a known identity. Failure does not roll
   back already completed writes or restore overwritten bindings.
5. Preserve original writes and data effects. Publish before the next guest
   observation of that completed range; do not insert a callback/safepoint in the
   middle of a logically indivisible binding update. Use the existing scheduler.
6. Only cache-coherent publication is in scope, as in the existing reference
   fixtures. Do not reproduce instruction-cache hardware or infer visibility from
   a random disc completion.
7. Every reachable code-changing operation must be covered by compiler analysis
   or a declarative writer summary. Conservatively unresolved writer effects are
   a strict-build error, not permission to install global store interception or
   silently retain byte inference.

The event API has two effects: `invalidate(destination, length)` and
`publish(module, placement, source_offset, destination, length, instance)`.
Generate their implementation in the game module. For a known completed write,
combine invalidation/publication into one non-observable update. Native loaders
use the same generated boundary; hosts do not choose modules independently.
Availability changes govern binding lookups; they do not revoke the compiled body
of an already active C invocation. Contract 4 defines that source-owned lifetime.

## Contract 3: partial code, dependencies and unknown states

Support build-known code versions and, for lifted implementations, instruction-
aligned compositions of their published ranges. Do not require every function or
image to arrive atomically. Supplied C is not replaced with lifted fragments to
implement a composition: use the source-level continuation mechanism in Contract 4
where that composition requires an explicit C control-flow transition.

Partition affected lifted control flow at entry points, replacement-range
boundaries and observable continuation points. Compile fragments from known
instructions ahead of time and bind their transfers explicitly. Keep ordinary
loops structured within stable fragments. A mixed function can therefore execute
B's published prefix and A's untouched suffix without pretending either whole
image is resident. No runtime instruction fetch/decode/execute loop is introduced.

- Preserve runtime table reads. Discover/compile the supported targets across
  the known inputs; do not freeze a mutable table to one image's target set.
- Do not inline across independently replaceable dependencies. Existing non-o32
  register-sharing transfers use the compiled continuation/frame mechanism below.
- Share equivalent implementations only when code and relevant assumptions agree.
  Module names or candidate order never resolve ambiguity.
- A binding lookup into incomplete/unknown instructions, an unregistered patch,
  an absent dependency or an unavailable entry must not choose stale compiled code.
  Fail at that transfer with its address and cause. Completed unrelated code remains
  callable. This does not invalidate an existing native C frame: its continuation
  follows authored C unless the source explicitly requests a different binding.
- A patch version explicitly supplied as a known compiler input is ordinary
  supported code. Do not synthesize executable code at runtime. Do not automatically
  register arbitrary values passed to a memory-write routine as code versions.

## Contract 4: source-owned C and lifted continuations

### Supplied C retains ordinary native frame semantics

Entering a C implementation selects that compiled C body for the invocation.
Activation changes subsequent slot-based calls, not the instructions or locals
of an existing C frame. Asyncify resumes that same authored C frame. This does
not snapshot guest data: later reads and calls still observe current data/bindings.

Do not reconstruct C locals from MIPS registers, replace the body with original
instructions, or require continuation annotations merely because a C function's
original address is reused. Keep authored functions and source locations in debug
builds; mark compiler wrappers artificial, not the source function. Source edits
must affect execution and breakpoints must refer to the C that actually runs.

### C code explicitly expresses a changing continuation

When the port requires switching the remainder of an active C computation, expose
that transition in C. Use an authored shared state struct and authored continuation
functions with the uniform form `uint64_t handler(State *state)`. Register them in
`dispatch.c_continuations` by logical slot, placement and original anchor address.
For a slot named `after_wait`, generate a typed declaration
`uint64_t psxw_continue_after_wait(State *state)` in `psxw_binding.h`.

The source explicitly calls this helper at its transition point. The helper
resolves the handler currently bound at that slot's anchor, checks the registered
state ABI, and calls the selected C function. It does not modify the caller's C
frame or replay a replacement prologue. Each handler owns its authored behaviour;
state is transferred only through the explicit struct, not a guessed locals map.
An ordinary resumed C function with no such call continues its own body.

Registrations for one slot must share the canonical state type/layout and return
ABI. Resolve C symbols from compiled source objects, not original MIPS bodies.
Missing handlers or conflicting state layouts are build errors. A reachable
lifted-to-C interior continuation additionally requires an authored C bridge that
constructs that state from the lifted continuation frame; no implicit register-
to-C-local conversion is allowed. Report `binding.missing_c_continuation` with the
slot, source function and missing handler/bridge. Do not substitute lifted code.

### Functions already assigned to the lift tier retain MIPS continuation semantics

Only for those functions, generate continuation entries at relevant calls,
safepoints and independently replaceable fragment boundaries. Save frame-local
live state covering known replacement continuations, using original register/stack
locations and conservative values where liveness is not proven. Resume using the
currently published continuation. Use a frame-local compiled-handle trampoline to
avoid recursion growth; there is no global CPU state or instruction interpreter.
Asyncify and the existing scheduler remain unchanged.

The existing lifted `D09/suspend-replaced` remains a required **0x22A** positive,
not **0x12A** and not a rejection. Add separate C tests: an ordinary C frame retains
its authored body after reload, while an explicitly authored C continuation can
produce the matching **0x22A** transition. Do not change the lifted fixture to the
native-C lifetime contract just to obtain a pass.

## Contract 5: callable representations and namespaces

B uses a separate native function table. Index zero is null. Native indices exist
only in compiler-proven native flows; guest references never enter that table by
numeric guesswork. Keep SDK/BIOS callback storage, `GuestCall.addr`, host invocation
and mod pointer interfaces guest-represented. Cross-tier native flows use typed
compiled wrappers. No tagged-pointer ABI or blanket structure widening is needed.

C replaces masked guest-call indexing with this exact software-callable policy:

1. Reject null and misaligned callable values before dispatch.
2. Recognize registered BIOS/kernel stand-ins and mod synthetic entries by their
   exact ABI values, in separate namespaces. An ordinary RAM address cannot alias
   one by sharing masked low bits.
3. Accept KUSEG, KSEG0 and KSEG1 views of the declared 2 MiB RAM callable space;
   canonicalize only these aliases, then require an available compiled entry.
   Other explicitly compiled text placements, such as scratchpad code, resolve by
   their registered addresses; never manufacture an entry from masked low bits.
4. Do not accept 2 MiB/4 MiB wraparound, unmapped addresses or unregistered synthetic
   values. A value inside the synthetic reservation works only if explicitly
   registered, never because its mask happens to select a RAM function.

This changes the callable ABI, not D5's data-address mapping or console hardware
exception behaviour. It resolves the D07 rejection cases as port-contract tests.
It does not restore whole-code validation or the removed D72 implementation.

## Contract 6: failure, compatibility and completion

Use deterministic diagnostics carrying the original address/site and relevant
module/range. Retain existing `untranslated` reporting where applicable and add
structured causes rather than accepting arbitrary traps:

| Cause | Required action |
|---|---|
| Missing writer/activation/reference facts needed for strict compilation | Build error naming the site and missing schema field |
| Required C write-site or explicit continuation mapping is absent | `binding.missing_c_write_site` or `binding.missing_c_continuation`; identify the source marker/handler/bridge to supply, never change tiers |
| Unknown selector or destination outside compiled placements | Reject the publication; never publish a guessed module |
| Invalid callable value | Reject before entering any target |
| Unavailable/unknown code or dependency | Reject at that transfer before executing incompatible code |
| Incompatible artifact ABI/schema | Refuse linking/loading with both versions reported |
| Unimplemented supported positive case | Keep the milestone/test failing and implement the missing mechanism |

A/B permit explicitly reported legacy regions during development. C adds
`[dispatch].require_explicit = true`; it rejects any executable/mod-policy consumer
still relying on resident-byte identification or incomplete writer coverage.
Guest pointers backed by explicit bindings satisfy this requirement. Native-handle
promotion is not required for opaque flows.

Keep the old mode only as an explicit migration option while inputs are converted.
Final acceptance requires all supported production inputs to require explicit
binding. No automatic fallback is allowed. Then delete the legacy execution/mod
selectors and their metadata. Publish dispatch input schema version 2 with explicit
binding as the default; reject `require_explicit = false` and version-1 inputs
with a migration diagnostic. Update the supported manifests to version 2.
Byte comparisons used solely for diagnostics or by original program logic are
not dispatch inference and need not be deleted.

## Evidence and maintenance

- Use existing build reports and `out/conform/dispatch/{report.json,summary.txt}`.
  Version them; list classifications, event facts, placements, actual source/tier
  provenance, C handlers/state ABIs, remaining legacy consumers and missing writer
  coverage, not just totals. C bodies must not acquire an implicit lift selection.
- Use original MIPS plus the live reference for original behaviour and matching C
  fixtures. Compiler annotations do not give the reference an activation answer.
  Deliberately edited C and native-C frame semantics use explicit authored-source
  expectations; do not discard edits to match an obsolete binary. Keep these tests
  distinct from unchanged lifted positives. Port-only callable namespaces and
  handles use explicit ABI expectations, not CPU exception claims.
- Extend the production fixture path; do not create another compiler/test framework.
  Keep a few integration cases per milestone and reuse D01–D10.
- Preserve failing observations and historical reports. Changing the mixed-code
  case from a proposed rejection to required execution strengthens coverage; obtain
  its original-MIPS evidence. Do not change positive expectations to avoid work.
- Missing tools, missing saves or absent input facts are failed prerequisites with
  explicit remediation. They do not become approval requests or passing skips.
- Update design/index/audits as implementation lands. Preserve unrelated work and
  replays. Keep mutations in the session scratchpad and generated artifacts ignored.

## Tasks

- [ ] Complete A and retain its evidence.
- [x] Complete B and retain its evidence (subset; [B progress](02-native-references.md#progress-log)).
- [ ] Complete C, production input migration and the full conformance gate.

## Progress log

- Plan authored; implementation not started.
- Replaced approval placeholders with concrete publication, representation,
  continuation, namespace and failure contracts.
- Removed automatic C-to-lift selection. Prescribed native C frame lifetime,
  authored C continuation/state interfaces and source-debugging acceptance instead.
- B implemented (D74): per-object native reference flows, the private native
  table and handle check (ABI 20), transport into lifted argument registers
  and back over a representation word, reports and the `D06/native-reference-*`
  group; acceptance pending review. The full corpus keeps its pre-B failing
  set for milestone C.

## Findings

- Function dispatch, active continuations and mod data-patch residency are distinct
  consumers of code identity. All are included in final acceptance.
- Existing standalone fixture guests do not establish real runtime-merge coverage;
  C explicitly supplies that integration.
