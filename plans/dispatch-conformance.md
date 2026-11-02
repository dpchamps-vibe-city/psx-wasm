# Dispatch conformance: executable semantics, not game coverage

**Status:** baseline runner and independent reporting implemented; remediation
and acceptance remain unfinished. This milestone is **not complete**.
D72 was removed, not adopted. Without `[dispatch]` facts, production retains
the distinguishing-word patch and the previous D6/D7 dispatch routing. With
them, dispatch-binding milestone A (D73) binds the covered regions explicitly.
Twenty-three `D05/explicit-binding-*` variants exercise that path; every report
names each variant's `explicit` or `legacy` path. The full corpus is 74 variants: 43 execution, 16 safety, 14 failed, 1 unresolved (exit 2).
The 14 failures and the unresolved variant are the legacy baseline set below.
The synthetic command deliberately returns nonzero on them. No mandatory CI gate is installed.

**Current contract and implementation direction:** the
[dispatch-binding plan](dispatch-binding/README.md) supplies concrete publication,
continuation, namespace and failure rules. It supersedes historical approval
blockers and proposed-policy labels below for those subjects; no further operator
decision is pending. Preserve the baseline evidence. Update fixture classifications
and required observations according to that plan as the contracts are installed,
not by relabelling incorrect execution as a pass. Supplied C remains authoritative:
ordinary C frames retain their authored body, and C continuation switches require
explicit source handlers/state. Preserve the existing lifted active-replacement
positive; add separate C-source/edit/debugging checks, never a hidden C-to-lift
fallback or a relabelled lifted result.

**Goal:** establish a game-independent, independently checked contract for
selecting and entering compiled guest code, including overlapping overlays.
**First delivery:** one thin integration runner and ten small, parameterized
synthetic fixture families. No general fuzzing framework or runtime redesign.

Read [conventions.md](../docs/conventions.md), [the architecture](../docs/emulation-layers.md),
[design §§2–4 and D5–D11](../docs/design.md), and
[TR2–TR4/TR6 in the shortcuts audit](shortcuts.md) before implementation.

## Context and limits of the current patch

The report is `games/bof4/replays/stuck-in-wrong-spot.rpl`: load a save, leave
the tent, enter the world map's `?` location, and spawn outside walkable terrain.
The investigation found that two scene functions at the same guest address
have identical first two instructions. First-match dispatch selected the wrong
translation. The current working-tree patch adds distinguishing code words.
The replay and focused tests passed after that change, but this is evidence
for that regression, not general dispatch correctness.

Relevant production paths:

- `crates/psxw-build/src/game.rs`: `prepare`, candidate construction and the
  patch's `overlay_checks`; `finish` for the production module pipeline.
- `crates/psxw-link/src/game.rs`: `entry`, `entry_from`, `dynamic_entry`,
  direct-call lowering, `Synth::OverlayStub`, and dispatch-table construction.
- `crates/psxw-lift/src/{program,graph,emit}.rs`: entry discovery, embedded and
  inlined regions, static callees, jump-table assumptions, and transfers.
- `crates/psxw-abi/src/lib.rs`: pointer indexing, o32 ABI and native stand-ins.

Gaps the suite must expose rather than copy into its expectations:

1. Separating known original images does not establish that the current
   executable state matches any of them. Mixed or modified bytes can pass
   sampled checks.
2. Identical intersecting text is not a proof of interchangeable translations.
   Inlined code, call binding and jump-table target discovery can matter too.
3. Shared function starts are not the same as overwriteable code regions.
   Unique entry addresses and main-container bypasses need coverage.
4. The patch's linker integration test supplies `GameSpec.overlays` manually;
   it does not test production candidate/check generation.
5. Existing function-level difftests replace callees with logged answers.
   Adapter native/device comparisons share dispatch machinery. Neither is an
   independent oracle for end-to-end dispatch.

The working-tree patch and replay are not assumed committed. Preserve them;
record their revisions/content hashes when establishing a baseline. Do not
turn the game-derived instructions into the synthetic corpus.

## Contract to review before fixing failures

> Within the declared supported execution model, transferring to guest address
> P must preserve the observable behaviour of the instruction-visible code at
> P and its calling state, regardless of container names or compiler layout.

Consequences:

- Names, candidate ordering and equivalent symbol partitioning cannot change
  results. Direct calls and indirect dispatch must agree where equivalent.
- All assumptions embedded in a translation must hold when used. Code identity
  is not merely an overlay filename or a matching prologue.
- Ordinary mutable data is read as data, not mistaken for overlay identity.
  Data folded into translation must be included in its validity assumptions
  or cease to be folded.
- Code arrival via a load, guest stores, `memcpy` or unpacking must not require
  a game-specific loader hook.
- Unknown executable states must not silently choose a plausible candidate.
  An explicit compile-time or runtime rejection is a safety result, not proof
  that the original program is supported.
- A partial load is not automatically invalid: execute supported reachable
  code correctly, or explicitly reject a state outside the supported domain.
  Do not require a fictitious atomic whole-overlay load to make tests pass.

Phase 0 must specify the supported domain: build-known code versions and
entry points, lifetime of compiled/inlined regions, address aliases, calling
conventions, replacement boundaries, and instruction-cache visibility.
Replacement of known code is distinct from arbitrary runtime-generated code.
Positive cases must not be reclassified as unsupported merely because they fail.

**Architecture guardrail:** this project is a static recompiler/native port.
The reference CPU runs only in tests. No production MIPS interpreter/JIT,
console-wide memory-map rewrite, instruction-count scheduler, or register-level
SDK detour is authorized by this plan. D69's residency redesign remains
explicitly deferred. If a failing case requires changing D5/D6/D7 or another
architectural decision, stop for operator approval; do not adopt that decision
through a test fixture. Likewise, full non-local-return support is not included.

## Test shape

Each fixture supplies original MIPS images, entry metadata, initialized memory,
a short loading/calling program, and an observation buffer. The same original
bytes execute on the pinned reference CPU and feed the compiler:

```text
original MIPS + initial state -> pinned emulator -> observations
             |
             +-> discovery -> lift / matching C -> production dispatch planning
                 -> game-mode link -> production optimization -> Wasm observations
```

- Derive candidates through the production planner, not fixture-authored
  fingerprints. Factor only the smallest shared preparation seam necessary;
  do not duplicate `prepare` or introduce a compiler framework.
- Keep calls inside compiled fixture programs. Host invocation alone misses
  direct-call, tail-call and interior-entry lowering.
- Start with lifted code; add small matching C companions for C/lift edges.
  Native callback/adapter seams use explicit fixture contracts. Synthetic
  native pointers are port ABI tests, not alleged hardware addresses.
- Compare initialized result fields, return words, ordered effect markers,
  caller-visible ABI sentinels, and relevant memory after each checkpoint.
  Do not require equal compiler spill layouts, all CPU registers, raw PCs,
  elapsed frames, or whole-RAM equality across native C and MIPS.
- On the reference CPU, synchronize executable writes with defined cache
  maintenance before calls. Cache-incoherent/self-modifying execution is not
  silently included. Timeouts/fuel exhaustion are failures, never matches.
- Reuse `psxw-oracle` target/provenance/report infrastructure and the pinned
  Beetle/OpenBIOS runner. Add only the bounded image/setup/entry/capture
  operation needed here; protect the mailbox, runner and stack ranges and
  bound runs at the host.
- The ordinary fixture corpus needs no game disc, save, SDK binary or decomp
  checkout. Commit authored fixture sources; generated binaries and reference
  outputs stay under gitignored `out/` or `.oracle/`.

## Initial corpus

Ten families, with a few named variants each—not a Cartesian product. Phase 0
assigns every variant an expected supported result or a reviewed rejection.

| ID | Family | Required variants / observable assertion |
|---|---|---|
| D01 | Prefix collisions | Two and three candidates; short and long common prefixes; late differing instruction; genuinely equivalent implementations. Correct effects, never name-order selection. |
| D02 | Translation dependencies | Same entry instructions with different reachable callees, inlined code or jump-table targets. Mutating ordinary data changes computation without falsely changing code identity. |
| D03 | Overlapping layouts | Unequal image lengths, different function boundaries, an entry present in only one image, main/overlay overlap, and two independent resident regions. Correct entry selection or a reviewed explicit rejection. |
| D04 | Replacement history | A->B->A; long->short with stale tail; chunked loads with calls at defined intermediate states; changed instruction outside chosen probes. Reachable mixed/unknown states cannot silently run an incompatible translation. |
| D05 | Arrival mechanism | The same executable state produced by host/native load, guest stores, a copy routine and a small unpacking loop. Equal results without per-game notifications. |
| D06 | Transfer routes | Direct/indirect C calls, lifted `jal`/`jalr`, tail transfers, interior entries and delay-slot effects. A route must not bypass code validity or alter call ordering. |
| D07 | Addresses and namespaces | Supported aliases, misaligned/unmapped pointers, dispatch-index collisions, native BIOS/kernel stand-ins. Invalid pointers must not be rounded/masked into unrelated valid calls; port-only namespaces tested against the ABI contract. |
| D08 | Calling state | C->lift, lift->C and indirect wrappers; register and stack arguments, `v0/v1`, stack restoration and caller-visible preserved state. Reuse existing marshalling cases rather than build a second ABI suite. |
| D09 | Re-entry and suspension | Native-to-guest callback, nested invocation, and a real supported suspend/resume with reloads between calls. Include a retained-code positive case and classify replacement of active code explicitly. No scheduler redesign. |
| D10 | Failure and ambiguity | Unknown reachable code, multiple incompatible matches, missing code/entry metadata and untranslated targets. Assert the intended diagnostic and absence of incompatible guest effects, not merely any Wasm trap. |

For suitable variants, change container names/order, legal load addresses,
equivalent symbol boundaries, prefix length and irrelevant stale memory.
These transformations must preserve observations. Use deterministic bounded
seeds, record each seed, and retain a standalone reproducer for every failure.
There is no requirement to prove arbitrary binary equivalence: identical safe
implementations may share an entry, but unresolved ambiguity must not be
silently settled by ordering.

## Execution phases

### Phase 0 — Contract and baseline

**Status:** baseline recorded; approval exit condition not met. No architectural
change is inferred from the contract or from a red test.

- [x] Inventory dispatch entry/bypass paths and translation dependencies;
  record current behaviour and proposed support classifications below.
- [x] Preserve the patch and replay; record current-patch baseline and focused
  tests. Previously reported replay passes remain historical.
- [ ] Review the proposed contract with the operator where it changes existing
  design assumptions. Record approved changes in `docs/design.md`; keep new
  architectural proposals separate from the conformance implementation.
- [x] Identify related two-word consumers (`psxmod.resident`, data-patch
  residency) in Findings. Do not claim function dispatch fixes them.

**Exit:** written coverage/support matrix, baseline evidence and approval for
any necessary contract changes. No coverage classification inferred from the
games' current behaviour.

### Phase 1 — Thin production-pipeline runner

**Status:** runner available; compiler failures remain red as permitted here.

- [x] Synthetic integration coverage in `psxw-build/src/dispatch_conformance/`
  and `tests/dispatch.rs`, above lift/link without a new crate or cycle.
- [x] Share production C preparation, lift compilation, overlay planning and
  optimization. Discover functions from fixture symbol metadata.
- [x] D01 and core D03/D04 cases use small closed-form outcomes and guest-side
  callers. Preserve failing cases and diagnoses.
- [x] Compare linked and `wasm-opt -O2` output. No flags suppress normal call
  optimization. The fixture guest links without the runtime; see limitations.

**Exit command:** `cargo test -p psxw-build --test dispatch`.
Compiler bugs may remain red; record them rather than weaken assertions.
`PSXW_DISPATCH_CASE=D01` selects the collision controls, not a full pass.

### Phase 2 — Independent reference and reporting

**Status:** live baseline available; unresolved classifications remain visible.

- [x] Bounded `OP_GUEST_CALL` runner operation and wire encoding, appended after
  existing runner functions. Setup/capture ranges are confined to scratch,
  instruction cache is flushed, and host target execution is bounded.
- [x] Original MIPS checkpoints checked against their authored closed forms
  before trusting their comparison with Wasm. Reference disagreement is not
  accepted as a new expected result.
- [x] `psxw conform dispatch [--case ID]` emits `report.json` and `summary.txt`
  under `out/conform/dispatch/`: ids, seed, classification/review requirement,
  checkpoint observations, differences, fixture hash, compiler revision,
  toolchain pins, reference provenance, and actual planner checks/tiers.
- [x] The command requires the live reference. Missing/failed reference evidence
  produces a non-passing report; a partial selection is explicitly labelled.
  No goldens or compiler-derived expected values.

**Exit command:** `psxw conform dispatch --case D01`.
A successful subset is not completion of the whole plan.

### Phase 3 — Adversarial coverage and sensitivity

**Status:** bounded D01–D10 baseline present; not complete. Stop for contract
review before production remediation or further test expansion.

- [x] Retain the 46 authored cases; apply name/order/address/stale-memory
  transformations to one collision control and name reversal to ambiguity:
  51 variants, not the previous 111 repeated variants.
- [x] Reuse existing marshalling, saved-native-entry and Asyncify machinery;
  C/lift, callback and suspension fixtures exercise their overlay interactions.
- [ ] Demonstrate sensitivity with isolated deliberate defects. The earlier
  D72 mutation campaign is historical, not validation of this corrected tree.
- [ ] Complete equivalent-symbol-partition coverage and review each fixture's
  support/rejection classification before claiming the required matrix done.
- [x] Keep compiler, missing-reference and unapproved-contract findings visible.
  Active-code replacement is a mismatch requiring review, not a passing or
  silently excluded `unsupported` case.

### Phase 4 — Gate and game regression

**Status:** not complete; gate deferred until the earlier approval blocker is
resolved. The mandatory gate remains a plan task, not a silently installed
requirement based on unapproved semantics.

- [ ] Add the mandatory `dispatch` gate to `psxw ci <game>` for the reviewed
  full corpus, not skippable from a manifest. Missing prerequisites must fail.
- [ ] Check gate plumbing with an absent prerequisite and injected mismatch.
- [ ] Run the save-backed BoF4 replay with its explicit card input and semantic
  checks. The replay is preserved, but a configured save and game facts for
  spawn/movement assertions are still missing; no fresh acceptance is claimed.
- [ ] Run existing title-to-field checks and both games' normal CI commands.
  Both games were rebuilt without D72 during cleanup; builds are not replays.
- [x] Correct design/index/audit claims: D72 is not adopted, TR4/TR6 are not
  resolved by discarded hashing/validation work, and this milestone is open.

**Exit commands:**

```sh
psxw conform dispatch
psxw run bof4 --replay title-to-field --check no-trap
psxw ci btn
psxw ci bof4
```

Also run the configured stuck-in-wrong-spot regression with its explicit save
fixture and semantic assertions. It is not a prerequisite for running the
synthetic compiler suite. Record a missing save as blocked game acceptance,
not a skipped synthetic conformance case.

## Completion criteria

- The suite runs independently of both games through production preparation,
  dispatch generation and linking, against an independent original-MIPS run.
- Every approved positive case matches observable semantics; every negative
  case fails for the specified reason before incompatible guest execution.
- Name/order transformations and applicable deliberately broken dispatchers
  demonstrate that tests protect semantics, not the present implementation.
- Reports distinguish execution agreement, expected safety rejection, failure
  and unresolved evidence. Missing required coverage cannot yield a green gate.
- No unapproved production architecture change or game-specific compiler rule
  has been introduced. Known limitations remain visible and attributed.

## Current baseline and review blockers

### Execution model and entry paths

The baseline compiles build-known images and discovered entries. Positive
fixtures use o32 boundaries, runtime data loads, synchronized executable writes,
and retained active translations; native stand-ins/callbacks and Asyncify use
explicit port contracts. No MIPS CPU is added to production.

| Route | Baseline behaviour |
|---|---|
| Dispatch table, lifted/C function pointers, host invocation | Masked D6 index; shared addresses select by sampled original text words |
| Lifted direct calls and tail/fall-through calls | `entry_from`; shared non-main addresses use stubs; main-container calls are exempt |
| Cross-container dynamic calls | Shared-address stub; known unique entries can be entered without residency checks |
| C direct calls | Direct when allowed by the C/o32 rules and main exemption; shared non-main addresses use stubs |
| Hook heads | Candidate's head is selected through the same dispatch route; overlay hooks not covered by this baseline |
| Saved BIOS/kernel stand-ins | Existing thunks and masked namespace; no new pointer validator |

Translation dependencies include reached instructions, expanded callees and
jump-table target discovery. Full text samples distinguish known overlays;
they do not prove every dependency valid. No full-footprint hash is present.

### Classification and results

The fresh run has **51 variants: 34 execution passes, 2 rejection passes,
14 failures, 1 unresolved**. The reference ran the supported checkpoints of
43 variants; all matched their closed forms. Invalid/rejection checkpoints are
port-contract tests, **not emulated console exceptions**. Proposed rejection
rules carry `review_required`, even if an observed rejection agrees.

| Area | Finding / review needed |
|---|---|
| D01, D02 callee/inlined/permuted-table/data | Passing controls; authored prologue replaces the game-derived pair from the old suite |
| D02/jump-table-disjoint | Positive failure: identical text with different reachable table targets produces `bad_jump` after reload |
| D03/main-overlap-direct | Positive failure: direct executable call still runs its old implementation; pointer route observes the overlay |
| D03/boundaries, D10/unique-unloaded, D10/untranslatable | Proposed safety failures: unique or untranslated resident code can enter an incompatible compiled candidate |
| D04/chunked-mixed, D04/patched | Proposed safety failures: sampled words do not detect the changed remainder; ordinary complete chunks/stale tails pass |
| D05, D06, D08 | Arrival, transfer-route and calling-state controls pass |
| D07 | Misaligned, 4 MB collision, unmapped and kernel-collision pointers fail the proposed rejection rule; 2 MB rejection agrees but remains unresolved pending contract review. Aliases and native stand-in control pass |
| D09 | Callback/nested/retained suspension pass. Active replacement resumes old code (0x12A vs proposed 0x22A); classification requires review and the mismatch remains red |
| D10/ambiguous, @swap-names | Both fail: returns 0x105 vs 0x100 depending on name order instead of rejecting absent dependency code |
| D10/nothing-resident, D10/no-metadata | Expected rejection with no incompatible effects |

**Approval blocker:** correcting these findings is not permission to change
D5/D6/D7. In particular, strict unknown/mixed-state rejection, executable overlap
routing, active-code lifetime and pointer-namespace rules need explicit review.
Do not infer approval, convert positive cases to unsupported, or restore D72 to
turn the report green. Further compiler changes are stopped here.

### Evidence and provenance

- Original baseline: `bceacc3` plus the user's patch, source-diff SHA-256
  `5afcfbf9ca2fd7f050a5efa843c5c678df7a8b565e001a88e070a4a5058ba45f`
  (recorded by the previous agent, not independently reproduced).
- Replay SHA-256:
  `5803ce1ca08689ca67e3ab25524d338d519d59ac6dc8ba40c00b9d1f0ee52fe9`.
- Fresh baseline: `cargo run -p psxw-cli -- conform dispatch`, exit **2**;
  per-checkpoint observations and fixture hashes in
  `out/conform/dispatch/report.json`. Target:
  `emu-beetle-7fea57a-openbios-8a86b7e-runner-1`.
  The three-file compiler/regression diff is preserved as
  `out/conform/dispatch/baseline-source.diff`, SHA-256
  `8535e0443155d9917f4a31627f25ec86e315454bb120e03655416809aa6fd57f`.
- The original two focused regressions and all eight linker-pass integration
  tests passed during cleanup. The final four `psxw-build` library tests and
  oracle committed-header test pass. Missing-reference command validation
  exits 2 and writes an unresolved report with the required setup command.
- Both game outputs were rebuilt without D72: btn 2,604,299 bytes and bof4
  10,257,868 bytes, matching the recorded pre-D72 sizes.
- The previous 111-variant / 24-failure baseline and D72's claimed green runs
  belong to removed commits `133a9a4` / `551a120`; they are historical, not
  evidence for this tree.

## Findings and limitations

- The contract is an approval boundary, not an instruction to make red tests
  green. Missing approval is not milestone completion.
- `psxmod.resident` still checks two words for script-mod data-patch residency;
  this suite/fix does not claim to repair that separate consumer.
- The fixture runner shares C preparation, lift compilation, overlay planning,
  thread-switch detection and optimization through crate-private functions;
  it does not run the runtime merge or hardware/adaptor policy selection.
- The suite does not yet cover overlay mod-hook chains or adapters at replaceable
  addresses, and equivalent-symbol-partition sensitivity remains open.
- Reference results cover only executable positive checkpoints; they cannot
  approve port-only rejection policy. Fuel/timeouts and arbitrary traps are
  failures, not substitutes for the intended diagnostic.
- No game replay or full game CI pass is claimed. Missing save data is separate
  from the synthetic compiler findings.

## Progress log

- 2026-10-31: plan created from the collision investigation.
- 2026-10-31: previous implementation introduced unapproved D72 and declared
  completion. Those completion and approval claims are withdrawn.
- Cleanup: initially reduced the task to a revert; the operator corrected that
  scope. Restored execution of this plan, retained the sampled-word fix,
  removed D72, bounded the corpus, and ran a fresh independent baseline.
  Production remediation and the completion gate stop at the approval blocker.
