# B — Native references where proven safe

**Status:** implemented (B1–B3; review rounds 1–4 applied); acceptance pending review. **Depends on:** [A](01-explicit-activation.md).
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

- [x] Extend A's binding plan with reference-site IDs and connected use-flow
  classifications. Include target identity, stability evidence, consumers and
  native/guest crossing information. Reuse compiler structures rather than a new IR.
- [x] Start with known direct calls, local address-taking through compiled helpers
  and labelled pointer initializers with enumerable consumers. Apply the eligibility
  algorithm above; do not promote unknown original numeric words.
- [x] Record a deterministic reason for retained guest flows: `late_binding`,
  `numeric_observation`, `opaque_escape`, `missing_uses` or `unstable_dependency`.
  Retention is not an error when explicit address binding preserves behaviour.
- [x] Extend relocation/resolver interfaces with site context. A symbol appearing
  in both a native and guest flow must receive different lowering at those sites.
  Changing `func_addr()` globally to return an index is not sufficient.

### B2. Emit native calls, handles and eligible data

- [x] Generate the private table and o32 entries described above. Keep generic
  hook-chain entry semantics; a known function identity does not authorize bypassing
  a mod's hook. Fixed direct calls retain caller-scoped hook handling.
- [x] Emit native direct/indirect calls and their existing-style marshalling
  wrappers. Do not change the argument/return ABI to solve identity transport.
- [x] Rewrite instruction relocations and data initializers consistently. Change
  only identified function-reference cells in eligible flows. Preserve the addresses
  and layouts of ordinary guest objects and all unrelated numeric values.
- [x] Carry native representation through eligible C/lift parameters and returns.
  Rewrite the consuming indirect call explicitly. If lifted arithmetic or another
  use observes the number, retain the entire flow as guest rather than smuggling
  a handle through an ordinary MIPS-address call.
  *As landed:* C source-only helpers' parameters and returns; C → lifted
  argument registers and back through a lifted return of the carried value,
  over a compiler-private representation word (see "Second review" below).
  Unproven boundaries stay guest as `missing_uses` and are listed there; they
  are not declared ineligible.
- [x] Leave opaque callback/runtime/host/mod interfaces unchanged. Test that a
  converted local reference cannot leak a table index into those guest fields.
- [x] Keep activation instrumentation intact through C optimization, Wasm rewrite
  and production optimization. Native pointer lowering must not erase A's events,
  C source locations or explicit source continuation calls. Treat a continuation
  helper as a changing slot, not a permanent reference to its current C handler.

### B3. Integrate evidence and validate

- [x] Add native-reference sites, retained-flow reasons and conversion boundaries
  to the versioned build/conformance reports. Distinguish guest explicit bindings
  from legacy byte inference; only the latter is unfinished identity migration.
- [x] Add a bounded `D06/native-reference-*` group through production preparation:
  fixed address-taking, an eligible relocated pointer table, a mixed C/lift flow,
  and a changing-slot control. Combine assertions rather than multiplying variants.
- [x] Include a numeric comparison/copy or opaque callback escape that prevents
  unsafe promotion. Verify its original pointer behaviour and that no native handle
  reaches the unchanged callback representation.
- [x] Assert emitted native table/call usage as well as observable results. A fully
  legacy dispatcher must not accidentally satisfy B's structural acceptance.
- [x] Compare matching-C/lifted fixtures in linked and optimized output with the
  live reference. Keep edited-C expectations source-authored as in A. Use explicit
  port-contract checks for handle representation, not console addresses.
- [x] Retain A's unchanged-pointer A → B and edited-C controls. Assert eligible
  native references enter the actual C definition, not a lifted duplicate. Mutate
  one native relocation in the scratchpad and demonstrate a targeted failure;
  restore it before acceptance.

## Exit criteria

- [x] Fixed direct, address-taken and relocated-data flows use the native mechanism
  through normal compilation, with structural and behavioural evidence.
- [x] A mixed-tier internal flow works with unchanged argument/return transport.
  An opaque callback/control flow keeps guest values and correct late binding.
- [x] Null/equality/numeric observations and existing hooks remain correct.
  Unproven flows are retained and explained, never guessed native.
- [x] A still passes, including preservation of edited C and source locations;
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

- Plan authored: native table, eligibility and conversion algorithms prescribed.
- Implemented (D74; `psxw-link/src/native.rs`, `game.rs`, `rewrite.rs`,
  `layout.rs`). A per-object flow analysis over the C tier's Wasm objects
  connects each reference site (`i32.const` with a table-index relocation; a
  table-index word in an AUX pointer table) to its uses: locals by definition,
  joined at control-flow joins (clang reuses parameter locals, so a flow-
  insensitive local would merge unrelated values), block results, `select`,
  source-only helpers' parameters/results, whole-word loads of a pointer
  table's cells. Eligibility and the five reasons are applied per connected
  flow, in that fixed priority order; targets are fixed only for main-image
  functions outside placements and overlay collisions, and source-only helpers.
- Lowering is per site: `Resolver::func_ref(symbol, site)` and
  `call_indirect_at(sig, site)` (defaults keep the old behaviour for the
  harness and mod links); `Layout::bytes_of` resolves each data cell by its
  relocation. `func_addr` is no longer the target-wide answer.
- `native_functions` follows the existing tables (1, or 6 in a moddable
  base), index 0 null, entries `Ctx::entry` (hook heads, operation wrappers,
  the C body's o32 wrapper) or a helper's `O32Wrap`. `Synth::NCall(sig)`
  marshals exactly as `ICall` and checks the handle before the call:
  `psx.binding_fault` cause 4, `binding.native_handle` (ABI 20).
- No native↔guest conversion is implemented: every flow that meets a guest
  boundary stays guest whole ("Otherwise do not promote that flow").
  `c_continuations` remain milestone C (A rejects them), so no continuation
  helper exists to classify yet.
- Reports: `build.json` schema 3 and conformance schema 3 carry
  `binding.native` (table, sites, targets with stability evidence,
  consumers, crossings, representation, reason/detail) and `references`
  counts `native` and `native-call`; conformance variants also carry the
  linked module's own table and native-call sites (`native`).
- Fixtures (`D06/native-reference-*`, through production preparation):
  `fixed` (helper return → equality → helper parameter → null test → native
  call into C and lifted targets; `escape` saves `&twice` where lifted code
  logs it, showing the original address; `numeric` observes `&thrice`'s low
  bits), `table` (AUX table of handles with a null cell; calling the null
  cell is `binding.native_handle 0x00000000`), `mixed` (C → lifted
  six-argument target natively; C → lifted parameter and lifted → C result
  stay guest), `slot` (explicit binding: the slot's address stays guest and
  follows A → B → A while the loads go through a native reference to the
  declared loader's operation wrapper; A's debug-location check runs on it),
  `edited-c` (closed form: the native call enters the edited C). Each asserts
  its handles, the body each entry enters, its native call sites (counted in
  the linked module and the report) and its native/guest flows.
- `link_passes::native_reference_keeps_hooks`: a handle to a mod-patched
  function enters the patch; the escaping flow stores `0x80011100`, not a
  handle.
- Acceptance (2026-11-01, live reference
  emu-beetle-7fea57a-openbios-8a86b7e-runner-1): `D06/native-reference` pass
  (4 execution, 1 safety); `D05/explicit-binding` pass (12 + 20, unchanged);
  full corpus 50 execution, 23 safety, 14 failed, 1 unresolved — the same
  failing set as before B (D02/D03/D04/D07/D09/D10 milestone C items).
  `cargo test -p psxw-build --lib`, `-p psxw-difftest --test link_passes`,
  `PSXW_DISPATCH_CASE=D06/native-reference -p psxw-build --test dispatch` pass.
  Reports kept apart under `out/conform/dispatch/b-acceptance/`.
- Mutation: making the first native code relocation name the next handle
  failed `fixed` and `edited-c` (twice's call entered thrice: log 0xf for
  0xa) and `mixed`/`slot` (`binding.native_handle 0x00000002` before any
  effect); `table` (data cells) was unaffected. Restored; the tree diff was
  compared byte for byte.
- Production: `psxw build btn` and `psxw build bof4` link. btn's 72 flows are
  all guest (60 `unstable_dependency`: overlays without placements; 12
  `late_binding`: changing slots); bof4's C takes no function addresses.

- Review fixes, first round. (1) A table load is a cell read only when it is a
  32-bit load at an offset proven word-aligned: values carry their residue
  modulo 4 (constants, addends, `shl`/`mul`/`and`/`or`/`xor`/`add`/`sub`,
  through locals until a join). The reviewer's byte copy straddling two cells
  (`memcpy(&x, (char *)ops + 4 * (i & 1) + 1, 4)`) was promoted and returned
  0 for 1; it now keeps the table guest (`numeric_observation`), regression
  `link_passes::native_reference_table_bytes_stay_guest`, which fails with the
  proof disabled (`i = 1`: 0, not 1). Accepted by review. (2) Lifted pointer
  calls whose target the lifter's dataflow proves (carried across an inlined
  callee's register passing or a join) are devirtualized
  (`Graph::proven_targets`; `D06/lift-devirtualized`). That is an adjacent
  optimization, not native-reference transport, and is described as such.
- Second review. (a) Devirtualization had changed hook semantics: a proven
  `jalr` became `psxw_f_`, which the linker binds with the caller's scoped
  hooks; the original pointer call enters the generic entry (reviewer:
  `use_ret` with a caller-scoped patch on `quad`, 20 vs 777). Every
  devirtualized `jalr`, window-constant ones included (the same deviation
  predates B), now emits `psxw_p_`: bound by `Ctx::pointer_entry` exactly as
  the dispatch table binds a pointer to the address (stand-ins, binding
  lookup, overlay stub, else the generic hook head), never caller-scoped.
  Genuine `jal`s keep `psxw_f_`. Regressions:
  `link_passes::devirtualized_pointer_call_skips_scoped_hooks` (777 for the
  direct call, 20 for the pointer call; 777 for both with the old lowering)
  and `psxw-lift devirtualized_pointer_calls_keep_their_binding`. (b) The B2
  transport capability, which (a) did not provide, is implemented as below.
  (c) The B2 checkbox no longer rests on a scope note.

- Checks after the second review (2026-11-01, live reference
  emu-beetle-7fea57a-openbios-8a86b7e-runner-1). Implementation status only;
  acceptance is the reviewer's. `D06/native-reference` pass (5 execution, 1
  safety); `D06/lift-devirtualized` pass (1); `D05/explicit-binding` pass
  (12 + 20). Full corpus 52 execution, 23 safety, 14 failed, 1 unresolved:
  the same failing set as before B (milestone C). Lift (18), link (10), build
  library and all 11 `link_passes` tests pass. Production, reported apart:
  btn and bof4 build; btn's 72 flows stay guest (60 `unstable_dependency`,
  12 `late_binding`), 2 lifted carriers, no transports or devirtualized
  calls; bof4 no flows, 1 carrier. Reports under
  `out/conform/dispatch/b-acceptance/review-2/`.

- Third review: the transport proof modelled instruction effects apart from
  what the emitter executes, and approved three unsound carriers (reviewer's
  `transport_review`): a load whose next instruction still read the old
  register stored the handle (`0x00000001` where the guest stores
  `0x80011100`); a `syscall` replaced `v0` (which `Insn::writes` doesn't name)
  yet the argument counted as returned (a C equality came out true); a move
  into `$gp` published the handle as the guest's gp. And the harness linker
  didn't lower the new intrinsics (`function psxw_rep_take has no address`).
  Now: while the value may be held, only `analysis::MODELLED` instructions are
  allowed, each emitted as exactly `Insn::reads`/`writes`
  (`emit::tests::transport_model_matches_the_emitter` checks the table, and
  the `PUBLISHED` registers, against `emit::stmt`); a move into `$gp`/`$sp` is
  an escape; a load in `facts.hazards` that overwrites it refuses; BIOS and
  kernel calls refuse. Regressions: `psxw-lift
  transport_refuses_unmodelled_effects` (the three reproducers, each failing
  when its rule is disabled, plus a positive control) and
  `link_passes::harness_links_carrier_translations` (a lifter-generated
  carrier through `Mode::Harness`; fails without the lowering). The harness
  lowers `psxw_rep_take` to 0 and `psxw_call_native` to a trap: it runs guest
  representation only.

- Checks after the third review (implementation status; acceptance is the
  reviewer's). The reviewer's `transport_review` (delay, syscall, gp) now
  refuses each carrier, harness-links, and the guest and native builds agree
  (stored word `0x80011100`, syscall result 0, gp `0x80011100` in both).
  Lift (20), link (10), build library (5) and `link_passes` (12) pass; D06 Wasm
  group passes. Live reference: `D06/native-reference` pass (5 execution, 1
  safety), `D06/lift-devirtualized` pass (1), `D05/explicit-binding` pass
  (12 + 20). Full corpus FAILS (exit 2): 52 execution, 23 safety, 14 failed,
  1 unresolved, the same 15 variants with identical verdicts and problem text
  as before B; none that passed before fails. Production, reported apart: btn
  and bof4 build; btn's 72 flows stay guest, 2 lifted carriers, no
  transports; bof4 no flows, 1 carrier. Reports under
  `out/conform/dispatch/b-acceptance/review-3/`.

- Fourth review (general): the fixes so far each closed a counterexample
  without a stated boundary. The account of both proofs' domains,
  invariants, rejection rules and limitations is now docs/design.md §2, "The
  acceptance boundary"; the code follows it. Auditing both proofs against
  what the emitter and the rewrite execute found more cases of the same
  kind, where something the proof didn't model was accepted by default:
  - Lifter: transfers the proof didn't name passed. `bltzal`/`bgezal`
    cleared `v1` on both paths although only the taken one calls. A new
    reproducer stores `v1` on the not-taken path: the old proof approved it,
    the new one refuses it. Other cases:
    - an inlined callee's `jr ra`, and its tails, return `v1:v0` to the
      caller when the target is 0, and no exit was recorded for them;
    - `setjmp` stores `s0`–`s7`, `fp` and `ra` to memory, and `rfe` unwinds;
    - a tail call through a register returns `v1:v0` when the target is 0,
      and `v1` wasn't checked.

    Now the transfer match is exhaustive, with no wildcard. While the value
    may be held, only a closed list of kinds is allowed; everything else is
    `missing_uses`. Every way out is recorded (`leaves`). A `jalr` consumer
    must be the plain dispatch-table call the emitter makes native.
    Continuations are refused inside the proof.
  - Shared facts: `Insn::writes` omitted a syscall's `v0`. Constant
    propagation (which devirtualization trusts) and the emitter's choice to
    capture a branch condition before its delay slot therefore disagreed
    with the emitted C. Now:
    - `Insn::writes` names `v0`;
    - the emitter's link writes use `Insn::writes`;
    - `transport_model_matches_the_emitter` checks reads and writes against
      `emit::stmt` for every plain instruction the decoder produces. It
      fails with the old `writes`.
  - Linker: any operator with a known stack arity took the numeric default
    arm, including atomics, SIMD, and table, reference and exception
    operators. `i32.const` relocations other than a table index or data
    address read as their placeholder: 0, which counts as null. Now:
    - a closed operator list, and per operator the relocation the rewrite
      lowers. Anything else makes the function unmodelled, which marks every
      table and helper it names by any relocation;
    - a table that overlaps another data symbol, or whose address appears in
      data by any relocation kind, is retained;
    - any table-index relocation counts as taking an address;
    - a helper called with another signature is marked.

    `only_plain_arithmetic_takes_the_default_arm` pins the operator
    boundary.
  - Devirtualized calls bind through `Ctx::dynamic_entry`, the existing
    binding by address. `pointer_entry`, a second copy of it, is removed.
  - An independent read-only audit of both proofs against the emitter and
    the rewrite found one more class: helper status was decided per symbol,
    not per function. A local alias of a function with an address
    (`__attribute__((alias))`) became a second identity with its own
    handle. Its native calls skipped the function's hooks, and `&alias ==
    &function` came out false (two handles where C has one function).
    Now only a function that one symbol names is a helper
    (`native::helper_function`). Regression:
    `link_passes::native_reference_alias_is_its_function`, which was
    promoted before the change (handles `t 0x80011400` and `t:run_local`).
    The audit found nothing else; its coverage notes are in this round's
    reports.

- Checks after the fourth review, on the final tree with the alias fix
  (implementation status; acceptance is the reviewer's). Reports under
  `out/conform/dispatch/b-acceptance/review-4/final/`; the audit is
  `review-4/independent-audit.md`.
  - Mechanism: unchanged in kind (handles, native table, representation
    word, devirtualization). The proofs accept less: what the boundary
    above lists.
  - Unsupported or unproven: the "Not proven, so not promoted" list in the
    acceptance boundary.
  - Focused checks pass:
    - lift tests (20, including the conditional-call reproducer and the
      every-instruction emitter check);
    - link tests (11, including the operator boundary);
    - build library (5) and `link_passes` (13, including the alias);
    - the reviewer's `transport_review` (delay, syscall, gp): each carrier
      is refused, harness-links, and the guest and native builds agree;
    - live reference: `D06/native-reference` pass (5 execution, 1 safety),
      `D06/lift-devirtualized` pass (1), `D05/explicit-binding` pass
      (12 + 20).
  - Full corpus: FAILS (exit 2), with 52 execution, 23 safety, 14 failed and
    1 unresolved.
  - Against the baseline: every variant's verdict matches the third round's
    run, the failure text is identical, and those are the 15 pre-B
    milestone C variants. None that passed before fails.
  - Production, reported apart: btn's and bof4's `game.wasm` are
    byte-identical to the builds before this round. Their reports are the
    same: btn 72 guest flows and 2 carriers, bof4 1 carrier. Neither game
    passes a handle, so this shows compatibility only.

### B2 transport: obligations, mechanism, evidence

| Obligation | Mechanism | Evidence | Remaining limitation |
|---|---|---|---|
| Native-reference production | C relocations only: an `i32.const` with a table-index relocation, an AUX table cell (`native.rs` sites); identity `(container, entry)` or a helper's `(container, TU, symbol)` | Exact sites in `binding.native.flows[].sites` (`<fn> code+0x..`, `<table> data+0x..`) | Not implemented: references lifted code materializes (`la v0, f`). Promotion is permitted where identity, binding and uses are proven (unknown numeric words are never guessed into handles); today they keep guest addresses, devirtualized where the target is proven |
| Internal transport, C | Source-only helpers' parameters and results unify their flows | `D06/native-reference-fixed` (`pick` return → `apply` parameter) | Helpers called through a pointer: their parameters/results are guest boundaries |
| Internal transport, C → lift | Lifter proves an argument register's entry value is only moved, null-tested and called through (`analysis::transport`), within the closed domain of docs/design.md §2, "The acceptance boundary"; such a translation takes a representation word on entry (`psxw_rep_take`), carried beside the unchanged o32 words as GCC's static chain is; a native C caller of the unhooked, unwrapped translation sets bit k for a handle in `a<k>` right before the call (`Synth::ToO32Rep`); guest callers leave it 0 | `D06/native-reference-lift-transport` `pass_lift` → `call_ptr` `a0`: `rep_` read at entry, `jalr` → `psxw_call_native` under bit 0; module: one `o32_call_native:call_ptr` call, one `native_call_lifted` call; a host call of `call_ptr` with a guest pointer runs the same body through the dispatch table; mutation clearing the bit → `untranslated 0x00000001` | Not proven (retained, `missing_uses`): a value the lifted function passes on to another call or tail call, holds across a BIOS, kernel, conditional, `setjmp` or `rfe` call or an inlined callee's return or tail, or calls through on some paths only; an argument outside `a0..a3` or part of a wider value (opaque escape) |
| Internal transport, lift → C | A carried value returned in `v0` on every way out is the C call's result (no conversion: it is the same value) | `pass_through` ← `ident`: the handle comes back and C calls it natively; the host's guest pointer comes back as its address | Returned on some ways out only: `missing_uses` |
| Consumption | C `call_indirect` → `Synth::NCall` (native table, handle check); lifted `jalr` → `bit ? psxw_call_native : psxw_call_indirect`, `psxw_call_native` → `Synth::NativeIndirect` (handle check, native table) | Linked module: native table entries, `native_call`/`native_call_lifted`/`o32_call_native:` call counts equal to the report's (`NativeBuilt`) | `jr` tail calls through a carried value: not proven |
| Semantic preservation | Binding: changing slots stay guest (`late_binding`); hooks: transport only into a translation entered directly (no hook chain, operation wrapper or slot entry), table entries are generic hook heads, devirtualized calls generic; equality: distinct functions at distinct addresses only; null: 0 in both; escape: a lifted store/syscall is `opaque_escape`, arithmetic/compare/address `numeric_observation` | `log_call` stores its pointer: `opaque_escape`, log shows the original address; `forward`: `missing_uses`; `link_passes` hook tests; `D06/native-reference-slot`; the three mutations | Outstanding work: the not-proven boundaries above; promotion of proven lifted-materialized references |

## Findings

- Clang's wasm register coloring reuses parameter locals for unrelated values;
  locals must be tracked per definition or a parameter's guest values poison
  every later flow through the same local.
- The C tier's census object already inlines `static` helpers, so they never
  become out-of-line helpers; a decomp's non-static helpers without an address
  are the source-only identities B promotes.
- btn's references all name overlays without placements or changing slots:
  native promotion there waits on placement facts, not on the mechanism.
- Native handle encoding is useful only where observable guest semantics permit it.
  Keeping an opaque guest value is compatible with complete retirement of inference.
- Ordinary C frames retain their source implementation. Milestone C supplies
  explicit source-owned C transitions and rebinding for already-lifted code;
  native pointer promotion never authorizes automatic C-to-lift replacement.
