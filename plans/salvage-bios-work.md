# Salvage the BIOS work

**Status:** in progress; recovery foundation integrated, BIOS completion and validation still open. General non-local returns are deferred by the user.
**Goal:** preserve the working native-port architecture, recover useful work from
spec-fidelity, and finish the missing BIOS functionality in Rust.
**Supersedes:** `plans/spec-fidelity.md` on `archive/spec-fidelity-b8a0683`
as the execution direction for this recovery and BIOS completion. That archived
plan remains historical evidence, not a checklist to resume.

## Read first

- [AGENTS.md](../AGENTS.md): smallest change, few tests, mostly integration.
- [Emulation layers](../docs/emulation-layers.md): the architecture to preserve.
- [Conventions](conventions.md): inputs, data hygiene, commands and commits.
- [Handoff](handoff-spec-fidelity.md): history and candidate commits. Its
  keep/remove classifications are leads to inspect, not authoritative decisions.
- [Design](../docs/design.md): affected sections and the D67–D71 salvage
  dispositions. The archived decisions were recorded without the user's input;
  only the explicitly reviewed recovery portions are retained.
- [psx-spx BIOS reference](https://psx-spx.consoledev.net/kernelbios/): retail
  A0/B0/C0 functions and syscalls. OpenBIOS and emulator observations are useful
  references, not automatically definitive evidence of Sony BIOS behaviour.

## The task, without expanding it

**Port the BIOS software to Rust, using the existing native backends instead of
recreating its hardware-driving machinery.** This is completion and recovery,
not a new BIOS, device framework, compiler rewrite or console-fidelity project.

The project already has files, memory cards, events, pads, threads, interrupt
handling, native backends and a virtual clock. Preserve them. A subsystem is not
missing merely because some BIOS entry points into it are missing.

Preserve function arguments, results, errors, guest-visible data, callbacks and
synchronisation. Use native disc, card, GPU, input and scheduling facilities for
the work. Do not introduce serial exchanges, controller command sequences or
instruction-count accounting inside the BIOS to achieve those results.

The existing per-chip device fallback remains. Shared IRQ/DMA support and shared
backend state are legitimate parts of that architecture; do not mechanically
remove every register-related operation. Guest-visible BIOS tables and callable
Rust thunks are also legitimate compatibility machinery, not CPU emulation.

### Scope limits

- Cover the retail PS1 BIOS's callable services, including its internal table
  entries. This does not add the Sony boot-menu GUI, devkit PC file server or
  PS2-only services.
- Keep static compilation: no CPU interpreter, runtime MIPS compilation or
  execution of untranslated code. `Exec`/`LoadExec` support for code included in
  the build is distinct from executing arbitrary new machine code.
- No instruction-based timing rewrite, console-wide memory-map rewrite, new
  generic backend abstraction or project-wide audit of "shortcuts".
- No game-specific heuristics or fitting timing constants until btn/bof4 pass.
- Do not reproduce private implementation details merely because Sony used
  them. Conversely, do not discard exposed state or callback behaviour merely
  because current games do not exercise it.
- Significant new architecture requires the user's decision before code. A
  `/goal`, a failing test or an old plan is not permission to make that decision.
- An unsupported diagnostic is not a completed working service. A blocked task
  stays blocked; do not redefine "full BIOS" to make the checklist green.

## Pinned recovery points

| Role | Revision |
|---|---|
| Pre-spec-fidelity baseline | `0bae88ac1bea1386c166b3c6f7a55bb07280b32f` |
| Audited spec-fidelity head | `b8a0683f70c7a8bb775bbaabb70b3c3def354b61` |

The latter is 168 commits after the former: 102 files changed, approximately
17,140 insertions and 4,050 deletions, including documentation. This is not all
BIOS work.

The handoff reports that the baseline passed the bof4 name-select crash replay,
and that the audited head passed that replay and `title-to-field` after the
TR2/TR3 reversions. These are reported results, not a fresh validation. Current
CI failures, including btn mods and oracle/fallback/browser failures, remain
unexplained; do not assume all are introduced by this work or already resolved.

At planning time these user files are untracked and must be preserved:

- `plans/handoff-spec-fidelity.md`
- `games/bof4/replays/craash-after-name-select.rpl`
- This plan, until deliberately committed.

Do not clean/reset the working tree or bulk-add its contents. Respect the data
hygiene rules when deciding what belongs in commits.

## Verified delta: what existed and what changed

| Area | Already present at the baseline | Useful work added since |
|---|---|---|
| Files/cards | CD file open/read/seek/close; card sectors, files, directory operations, formatting, persistence and completion events | General device dispatch, more file APIs, error fixes, card busy refusal and sector-boundary correction |
| BIOS C library | Basic strings/memory, random numbers, `printf`, `InitHeap` bookkeeping | Conversions, additional strings, allocation, sorting/searching and behaviour fixes |
| Events/interrupts | Events, waits, critical sections, handler queues, exception exit | `UnDeliverEvent`, RAM-backed event blocks, default-handler ordering and acknowledgement controls |
| Timers | Timer core and SDK access | BIOS timer entry points over that existing core |
| Threads | Open/close/change, guest TCBs and Asyncify suspension/resumption | No change to `psx-bios/src/threads.rs` or `psxw-link/src/asyncify.rs` |
| Pads | Native sampling and BIOS buffers | Correct old pad API, `PAD_dr`, start/stop and flag behaviour |
| GPU/disc | Native cores, SDK adapters, BIOS file reads and `GPU_cw` | Additional BIOS GPU and low-level CD entry points |
| BIOS tables | Table-address getters | Populated tables, patchable dispatch and original-function thunks |
| Boot | Disc executable loading and `SYSTEM.CNF` parsing | Stack-selection correction, boot argument and event-block configuration |

### Salvage, not wholesale copying

- **Retain useful BIOS software:** `libc.rs`, file/device API expansion in
  `files.rs`, event/kernel API corrections, pad API fixes, boot corrections and
  guest-visible tables. Inspect their dependencies and incomplete cases below.
- **Replace unwanted implementations, not their APIs:** CD register/DMA driver
  in `cdrom.rs`; GPU register/DMA path in `bios.rs`; card protocol exchanges in
  `card.rs`; pad protocol reads in `kernel.rs`. Use the already existing cores.
- **Recover independently useful fixes separately:** translator/compiler
  correctness, SDK shared-state fixes, renderer error propagation, BIOS oracle
  support and per-game browser disc storage. Use the handoff's commit list to
  locate changes; inspect each patch. Do not reintroduce the reverted TR2/TR3
  heuristics or their follow-up exemptions.
- **Do not carry forward phases 6/7:** the console-wide map, instruction-cost
  instrumentation and assembly-path spending throughout SDK adapters. Restore
  the baseline scheduling and address-window approach, with only independently
  justified corrections.
- **Keep other changes separate:** overlay residency is a recompiler change,
  not BIOS completion. Compiler optimisation flags and difftest domain changes
  also need their own justification; do not inherit them just because another
  selected commit depends on them. Record a keep/rework/defer disposition rather
  than silently losing or approving these changes.

## Current work and blockers

Checkboxes record the stated task, not a claim that every service in a family
is complete. Recovery and executing a gate can be complete while a service is
partial or that gate still fails. Detailed results below are checkpointed history.

| Area | Current disposition / next action |
|---|---|
| Native recovery | Integrated; original main and archived work preserved. Remaining service gaps are listed in section 3, not hidden under recovery. |
| `setjmp`/`longjmp` and dependent `_ioabort` | **Deferred by the user.** Record the incorrect frame-return behaviour; revisit later. No control-flow design or implementation in the current work. |
| Card, console and CD | Partial; CD blocking calls wake on live event flags, with ordered consumption and native cancellation. Returning read errors retry up to ten attempts. Seek/read/status honor patched A78/A7E/A7C commands and bounded acceptance retries, with per-call guest stack locals. Completion probes dispatch patched B0B TestEvent; long command-acceptance loops now yield through the existing loop budget so busy native work can progress. Remaining CD internals stay incomplete. CD opens preserve ISO versions and accept `?` filename matching. CD file reads enforce sector alignment/EOF and now use the existing native blocking helper, including patched commands/events/errors and scheduled completion. They transfer the full requested sectors and clip using live FCB metadata after success. |
| Other executable, exception and thread gaps | A(40h)'s documented fatal service is now named/handled/tested; BREAK event/exception entry remains open, as do executable and thread gaps. No runtime redesign authorised by the longjmp deferral. |
| Fonts and BIOS identity | Glyph-resource/identity decisions remain open. B53's resource-independent offset mapping needs better evidence: the proposed OpenBIOS table lookup aliases valid charset-3 codes and was rejected. No invented glyph pointers, dates or versions. |
| Validation | CD file-read Flow follow-up passes BIOS 58/runtime 11/SDK 42 tests and wasm32 check. Fresh native builds and all four unchanged replays pass for this batch; those replays do not call A60, whose new behavior is covered by the integration tests. Browser storage at `8e5badf` passes 25 checks each in Chromium/Firefox; ABI-18 BTN browser conformance and game/recording gates remain at `9624526`, before this native batch. Memcard conformance remains at `99fa333`; earlier full CI has classified failures and missing prerequisites, not a green verdict. |
| Documentation | Front checklist, design/archived-decision dispositions, plan index and native-BIOS open items reconciled. Older broad audit findings remain historical leads, not a new audit or a current unresolved-bug count. |

### Continuation lanes after deferring non-local returns

| Owner | Checkout / authority | Contract and next gate | Independence |
|---|---|---|---|
| Parent | `salvage/bios`, writer | CD live-event waits, returning read-error retries, patchable seek/read/status command acceptance and B0B event polling implemented. Long command-acceptance loops now yield for native busy-work progress. CD file reads now use the existing blocking helper, preserving its command patches, waits and returning errors; live-FCB clipping and zero-count failure are covered. Focused tests/wasm32 check and fresh builds/four replays pass. Remaining CD internals stay partial; flag-only chain-control proposal rejected. | Owns plan, integration and final acceptance |
| Documentation worker | `../psx-wasm-salvage-lanes/docs-reconciliation`, writer at `99fa333` | Complete: `05024b3` reviewed and integrated as `124d922`; parent corrections and open-items update follow | Separate worktree; no runtime/code/build changes |
| CD contract scout | Parent checkout, read-only at `99fa333` | Complete: confirmed event-wait gap; proposed late-only event-selection fix rejected after source review | No edits, tests, outputs in the repo, or architecture authority |

Workflow `1610e372-e898-4cf0-946a-0dc7f60bd941` completed both lanes. No active
children from that workflow, subdelegation or control-flow work. Source/gate
claims remain checkpoint-specific; worker completion alone is not parent acceptance.

The subsequent browser worker `7ccc8568-6c02-46fe-8c24-162d54aef56d` stopped
with four uncommitted files after its CLI build reported **No space left on
device**; the tool registry then returned `Async run not found`. Parent reported
the failure, verified no child/build was active and preserved the partial diff
as `browser-disc-interrupted.diff` in session scratch. No browser gate ran.

The same-tool retry, `828341b3-aa23-4b55-a23d-a3dcfe76bf23`, completed in
`../psx-wasm-salvage-lanes/browser-disc-validation` (base `aea8c94`) as
`e18f62e`. Parent rejected the earlier same-content test and sequential-write
fix, then reviewed the corrected four-file diff, complete report and browser
results before integrating it as `12c5fbb`. The new test uses different
replacement content, verifies old cue/bin/key survive lock refusal, and proves
successful import after unlocking. Parent `8e5badf` additionally releases
unused preflight writers on copy failure. The integrated storage gate passes
25 checks in each browser, detailed below. This lane is accepted and complete;
no active child, runtime/ABI change, subdelegation or new storage architecture.

## Work order

### 1. Recover a native foundation

- [x] Preserve the audited head on an archive branch. Recheck for later user
  changes before branching; do not overwrite existing branches.
- [x] Create a separate salvage branch/worktree from the pinned baseline.
  Preserve this plan, the handoff and the crash replay for use there. Leave main
  and the original working tree intact.
- [x] Recover independent correctness fixes in coherent batches. Compiler,
  lifter, libc, SDK/shared-backend and host batches were reviewed and integrated
  without the rejected map/timing chain; separate deferred changes are recorded.
- [x] Record dispositions for unrelated useful changes, including residency,
  browser storage and fallback-device fixes. Keep this separate from the BIOS
  completion checklist.
- [x] Build and check the recovered foundation using the existing commands.
  Builds and integration gates ran; full ABI-17 CI failures are classified below.
  Regression attribution remains unknown where no comparable baseline exists.
  This records execution and evidence, not a green full-CI verdict.

**Exit:** a buildable recovery branch retaining the original native/fallback
architecture, without phase 6/7. Nothing has been destructively reset or merged
back to main.

### 2. Salvage the useful BIOS changes

- [x] Recover C-library and allocation additions and existing-API bug fixes.
- [x] Recover event, timer, pad and card API improvements over existing native
  facilities, retaining Asyncify threading. Remaining partial behaviours are
  listed in section 3; recovery does not complete those services.
- [x] Recover guest-visible BIOS tables, events, handler chains and file/device
  blocks, including callable original/kernel thunks and patched SDK dispatch.
  Rust implements the functions; no executable Sony BIOS code is required.
- [x] Recover general file/device dispatch, including guest-provided device
  callbacks. Keep useful CD directory and error behaviour while replacing the
  controller-driven reads with native disc operations. Console input and card
  asynchronous visibility remain separate unfinished items below.
- [x] Recover added GPU and public CD operations over existing native cores
  and scheduling, without requiring SDK initialization or a BIOS register driver.
- [ ] Complete the remaining CD callable internals and partial software contracts
  recorded in the native-CD section below; recovered public operations alone
  do not complete the original GPU/CD task.
- [x] Restore native pad/card access with corrected buffers, busy/error states
  and completion events over shared native/fallback card/input state. Remaining
  helper and completion limitations are explicitly retained in section 3.
- [x] Recover boot fixes and BIOS oracle support with consistent ABI/linker/
  browser glue. ABI-18 builds are isolated from old outputs. Actual non-empty
  oracle comparison and remaining browser coverage are separate validation gaps.

**Exit:** useful BIOS coverage at the audited head is retained or explicitly
recorded as still pending, but no BIOS operation requires the rejected driver
mechanisms. Run focused existing tests as each family lands; do not wait for the
whole salvage to discover integration failures.

### 3. Finish the actual gaps

The inventories below describe **the audited head**, not the baseline. During
salvage, some entries will temporarily be absent again. Do not claim completion
until they are restored and their remaining gaps are addressed.

Work in small function-family batches: kernel/configuration, card helpers,
console, fonts, executable services and non-local control flow. Dependencies
such as `_ioabort` on non-local return must be made explicit. Use existing
helpers wherever possible.

#### A. Entries with no handler

At the audited head, all 29 slots below reached `not implemented` after alias
resolution. The checkboxes now track recovered/implemented entries; unchecked
entries may be partial rather than absent. Five are documented no-ops, and
A(52h)'s body still needs verification.

| Done | Entry | Function |
|---|---|---|
| [x] | A(41h) | `LoadTest` |
| [x] | A(42h) | `Load` |
| [ ] | A(52h) | PsyQ binds `GetSysSp` to this slot, but its returned pointer/body remain unverified and psx-spx conflicts; do not guess a stack address |
| [ ] | A(9Ch) | `SetConf`: main-thread reset implemented; live non-main reset still blocked |
| [x] | A(9Dh) | `GetConf` |
| [ ] | A(A7h) | `bufs_cb_0` |
| [ ] | A(A8h) | `bufs_cb_1` |
| [ ] | A(A9h) | `bufs_cb_2` |
| [ ] | A(AAh) | `bufs_cb_3` |
| [ ] | A(AEh) | `bufs_cb_4` |
| [ ] | A(B2h) | `ioabort_raw` |
| [ ] | A(B4h) | `GetSystemInfo`: size/flags implemented; date/version identity unresolved |
| [x] | B(4Dh) | `_card_info_subfunc`: native queued info, low event only |
| [ ] | B(53h) | `Krom2Offset`: independent of glyph resources, but the proposed reference lookup has valid-codepoint conflicts; verify mapping before implementing |
| [x] | B(58h) | `_card_chan`: last requested device ID, not slot index |
| [x] | C(04h) | `get_free_EvCB_slot` |
| [x] | C(05h) | `get_free_TCB_slot` |
| [x] | C(08h) | `SysInitMemory` |
| [ ] | C(09h) | `SysInitKernelVariables` (reference notes its post-boot corruption; verify the callable behaviour) |
| [x] | C(0Eh), C(0Fh), C(10h), C(11h), C(14h) | Five retail zero-return entries |
| [ ] | C(15h) | `_cdevinput` |
| [x] | C(16h) | `_cdevscan`: live FCB/device scan; built-in host input still absent (`Partial`) |
| [ ] | C(17h) | `_circgetc` |
| [ ] | C(18h) | `_circputc` |
| [ ] | C(19h) | `_ioabort` |

`LoadTest`, `Load` and the five zero-return entries were omitted from the old
handoff's missing-call list.

#### B. Named calls and paths that are incomplete

- [ ] **General `setjmp`/`longjmp`, A(13h/14h) — user-deferred bug.** Recognised
  lifted `setjmp` sites save registers and produce a continuation, but entering
  it as an ordinary call does not unwind intervening frames and eventually
  returns to the wrong caller. The direct Rust paths remain incomplete; existing
  exception-hook continuations do not establish general longjmp support. Record
  and revisit later, together with dependent `_ioabort`; no design or runtime
  implementation is authorised by the instruction to continue independent work.
- [ ] **`Exec`/`LoadExec`, A(43h/51h).** Both always error, even for code already
  translated. Reuse executable-loading and guest-call facilities for build-known
  executables, including their argument, stack, GP and return behaviour. Loading
  bytes and admitting executable code are separate operations.
- [ ] **Warm boot, A(A0h).** Initial boot exists, but this entry always errors.
  Implement the kernel reset/restart behaviour over the existing boot/runtime
  facilities, without introducing a BIOS GUI or machine-code execution.
- [ ] **Font access, B(51h).** `Krom2RawAdd` always returns -1. Implement mapping
  and accessible glyph data together with B(53h). Establish a legally usable
  resource source with the user; do not commit a BIOS dump or extracted font,
  invent a valid pointer to missing data, or call this complete without glyphs.
  B53 itself needs no glyph resource, but the inspected reference's original-font
  lookup aliases valid charset-3 codes; its mapping remains unverified separately.
- [x] **`SetMem`, A(9Fh).** Updates the guest-visible RAM-size variable for
  retail's 2/8 MB choices, also reported by GetSystemInfo(5). No host memory
  resizing or console-wide bus map.
- [x] **Card auto-format, A(ADh).** Opt-in formatting of an invalid MC header,
  exact previous setting return and `_bu_init` flag reset are implemented;
  missing and valid cards are untouched. Tests use disposable in-memory cards.
- [ ] **Card waiting/completion.** `_card_wait` now blocks on scheduled vblanks;
  async file frames become visible at completion, with busy/error state and
  read/write-specific event order. Generic close releases its FCB even on error;
  native cleanup cancels old queued work to protect descriptor reuse. Explicit
  A(A7h..AAh/AEh) callbacks remain partial. A8/A9/AA now end pending Info/Load
  high-level notifications without consuming the driver's scheduled completion;
  write-test notifications/effects, AE's body and unspecified returns remain
  unresolved. OpenBIOS does not implement AF; absence of its body is not evidence
  for either high-level notifications or their removal.
  No serial exchanges or instruction timing added.
- [ ] **Console input/control.** `gets` now edits and echoes a bounded line
  through guest file/device callbacks, including quoted characters and deferred
  termination. C16 now scans live flagged FCBs through guest device ioctls,
  including before read/write device lookup. Built-in blocking TTY reads still
  error; host input, circular helpers and low-level TTY control remain unfinished.
  A working guest-device scan is not built-in host-input integration.
- [x] **Direct SYS(03h).** The TCB-address syscall now uses the same pending
  switch, SP/GP bookkeeping and Asyncify stacks as B(10h). Build scope includes
  syscall sites; the syscall import stops rewind without repeating the switch.
  Native thread lifecycle and real clang/link/Asyncify resume tests pass.
- [ ] **Thread limitations.** Review and resolve the rejection of switching
  from callbacks/interrupts and the limited externally editable TCB context.
  Do not replace working Asyncify threading or invent a simulated CPU register
  file without a separately approved design.
- [ ] **Exception and hook paths.** Normal IRQ dispatch and recognised
  `ReturnFromException` already work. C(06h) chaining is rejected; `break` merely
  logs rather than delivering the unresolved-exception event/error. A(40h)
  itself now has its documented fatal handling and patchable table dispatch;
  this does not implement BREAK's exception entry/return path. Native
  kernel-handler pointers are now callable, and libapi/libetc/libpad/libgpu and
  libmcx calls honor BIOS patches, including libmcx's vblank handler. Complete
  the remaining exception paths through existing driver, guest-call and lowering
  seams; do not infer a general exception context.
- [x] **Coverage reporting.** Build reports distinguish working, partial,
  unsupported, documented fatal/no-op and dynamic unknown calls. The old
  `implemented` field remains a compatibility gate, not a completeness claim.
  Alias names resolve consistently; classifications must follow later changes.

For control-flow changes that require a substantial compiler/runtime redesign,
record the concrete missing capability and ask the user before implementing it.
Other independent gaps may proceed. The blocked capability remains visibly
unfinished; neither a diagnostic nor approval of an exclusion means it was
implemented.

#### C. Do not invent additional work

- `atof` and `strtod` use absent COP1 on the real console. Their documented fatal
  behaviour is not a request to add working floating-point conversion services.
- `FlushCache` need not simulate a cache that does not exist in this runtime.
- Documented zero-return/fatal entries need the appropriate behaviour, not
  fabricated functionality.
- A different private algorithm, absent wire protocol or redundant cache is
  not itself a BIOS gap. Fix demonstrated software behaviour, not resemblance
  to the original implementation.

**Exit:** missing entries and partial behaviours are implemented and checked,
or individually recorded as blocked with a reason and next action. The phase
and the full-BIOS goal are not "done" while required functionality is blocked.

### 4. Validate and reconcile the documentation

- [x] Run workspace tests and build both games from the recovery branch's own
  toolchain/artifacts. Latest workspace run: `6deb05b`; subsequent batches have
  focused tests. Latest release/runtime/game builds: CD file-read Flow
  batch below, ABI 18 (includes acceptance progress/TestEvent dispatch). Census
  and optional-test qualifications are recorded with their actual checkpoints.
- [x] Run focused integration tests for changed families, including guest devices,
  card completion, patched BIOS calls and real wasm kernel/thread lowering.
- [ ] Run non-empty BIOS oracle comparisons. The earlier zero-vector command is
  not validation; configured images/vectors/goldens are still missing. Keep
  emulator/OpenBIOS results provisional where appropriate.
- [x] Run existing BTN title/card-load, BOF4 `title-to-field` and supplied
  name-select crash replays: latest source is the CD file-read Flow batch
  below, with unchanged expectations (includes acceptance progress/TestEvent dispatch).
  Card-load has no state assertions and does not certify save persistence.
- [ ] Validate game-level save/reload persistence once the required recordings
  and state assertions are available; the current card-load gate is insufficient.
- [x] Check native/fallback integration with the existing gates: full chip/SDK
  gates at ABI 17, focused libpad/libetc/libgpu and memcard gates at ABI 18,
  latest memcard conformance at `99fa333`. Results are not all at one checkpoint.
- [x] Validate two-bundle browser disc isolation, locked-disc refusal and unlocked
  replacement through production pages/OPFS. Chromium and Firefox each pass 25
  storage checks at `8e5badf`, retaining all seven card checks.
- [x] Refresh BTN browser conformance and game/recording gates at ABI 18:
  Chromium/Firefox pass five scenarios and the 6,000-frame title replay with
  native RAM/VRAM/audio comparisons at `9624526`, detailed below.
- [ ] Complete remaining mod/GPU-oracle browser gates: BTN mods census inputs
  and GPU vectors/goldens remain missing. Cross-host agreement is not an oracle
  comparison or a full-CI pass.
- [x] Run `psxw ci btn` and `psxw ci bof4` and classify failures: both completed
  at the ABI-17 foundation and exited 2. This is execution, **not a green gate**.
- [ ] Resolve/provision the recorded full-CI blockers and run a current-source
  full gate when meaningful. Do not repeat empty oracle checks or silently
  change difftest input domains, exclusions, goldens or replay expectations.
- [x] Update affected design sections, D67–D71 dispositions, the plan index and
  native-BIOS open items. Full BIOS/CI remain unfinished; older audit entries
  are labelled historical rather than silently treated as current bugs. This
  does not claim every unrelated historical finding was re-audited.
- [ ] Present the recovery branch and remaining limitations for review before
  merging into main. Do not delete the archived work.

## Validation discipline

Use the existing infrastructure. A small integration test per changed family or
concrete regression is preferable to a new harness or one trivial test per
entry. Important cases include a guest device callback, patched BIOS dispatch,
card busy/error/completion behaviour, executable return and non-local return.
Use isolated storage for card tests.

Core commands, using the freshly built CLI:

```sh
cargo test --workspace
cargo build --release -p psxw-cli
./target/release/psxw build btn
./target/release/psxw build bof4
./target/release/psxw oracle check btn --device bios
./target/release/psxw run btn --replay title-to-gameplay
./target/release/psxw run btn --replay card-load
./target/release/psxw run bof4 --replay title-to-field
./target/release/psxw ci btn
./target/release/psxw ci bof4
```

Do not run every long command after every edit. Run local checks per batch and
full gates at meaningful integration points. Do not weaken oracle expectations
or re-record replays merely to make changed behaviour pass.

"All chips native" and `no-io` only describe the checks they actually make.
Internal Rust register accesses are not counted as guest I/O, so passing those
checks alone does not prove the BIOS uses the intended native path. Review the
changed call paths; do not build a new monitoring subsystem for this.

## Completion criteria

- Working services from the baseline are preserved.
- Useful BIOS additions and verified correctness fixes are recovered.
- The rejected memory-map and instruction-accounting expansions are absent.
- BIOS hardware operations use the native services, with shared state and
  existing fallback support intact.
- The table inventory and partial-function list reflect tested reality, not
  merely recognised names. Required unresolved items prevent a "full BIOS"
  completion claim.
- Tests, integration results, documentation and the review branch agree.

## Execution setup

- Archive: `archive/spec-fidelity-b8a0683` at the audited head.
- Recovery branch: `salvage/bios`.
- Recovery worktree: `/home/dave/Documents/code/psx-wasm-salvage-bios`.
  This is the implementation checkout; the original `psx-wasm` checkout remains
  on main and its plan copy is the original draft.
- The user's updated `AGENTS.md`, this plan, the handoff and the crash replay
  were copied into the recovery worktree. The original modified `AGENTS.md`,
  untracked `.pi/`, plans and replay were not altered. Existing worktrees were
  left alone.
- Delegation is explicitly authorised. Prefer
  `runinfra/deepseek-v4-1-flash-1017` for parallel research and bounded
  implementation, and `qwen36-llama/qwen3.8-27b` with low/medium reasoning.
  **At most one Qwen run at a time.** Confirm live model identifiers through the
  subagent model list when available.
- Each mutation-capable worker gets a separate worktree and a bounded contract.
  The parent owns architecture, difficult control-flow integration and final
  acceptance. First delegate independent compiler fixes, SDK-state review and
  BIOS library salvage; do not give multiple workers the shared BIOS dispatcher
  in the same worktree.
- Tooling blocker resolved: the session now exposes the native subagent tools.
  No alternative CLI/SDK agent runner was substituted.

### Initial delegation wave

All writers start from `c510849` and commit only inside their own worktree.
No child may push, merge into main or expand the approved architecture.

| Lane | Model / authority | Owned seam | Worktree | Gate / handoff |
|---|---|---|---|---|
| Lifter | DeepSeek / writer | `psxw-lift` independent correctness fixes | `../psx-wasm-salvage-lanes/lift`, `salvage/lift` | crate tests; reviewed commits |
| C lowering | DeepSeek / writer | Standalone `psxw-cc` correctness fixes, excluding cross-crate ABI work | `../psx-wasm-salvage-lanes/cc`, `salvage/cc` | crate tests; reviewed commits |
| BIOS C library | DeepSeek / writer | Native libc/heap/printf salvage and its BIOS dispatch arms | `../psx-wasm-salvage-lanes/libc`, `salvage/libc` | BIOS tests; reviewed commits |
| SDK state | Qwen, low / read-only scout | Dispositions and dependencies of SDK shared-state changes | original checkout, read-only | source-backed bounded report |
| Integration | parent / writer | Link/runtime integration, guest-visible kernel interfaces and final acceptance | recovery worktree, `salvage/bios` | integrated tests and source review |

These are independent source contracts. Shared dispatcher conflicts are resolved
by the parent at integration; workers do not edit one another's trees. Further
Qwen work waits for the scout to finish.

Wave `cc86f2b4-4a1b-478d-9efd-3616d1d8452e`: all three DeepSeek writers have
completed and their source diffs were reviewed and integrated. Qwen's SDK scout
also completed. Detailed handoffs are in this session's subagent artifacts,
under `outputs/cc86f2b4-4a1b-478d-9efd-3616d1d8452e/salvage/` (`cc.md`,
`lift.md`, `libc.md`, `sdk-state.md`). Its SDK dispositions are research leads,
not accepted wholesale: phase 6/7 do touch these files, contrary to the report's
opening assertion. Inspect original commit boundaries, not current whole files.

### Second delegation wave

| Lane | Model / authority | Owned contract | Worktree / branch | Gate |
|---|---|---|---|---|
| Division | DeepSeek / writer | Source-placement division normalisation through linker imports | `../psx-wasm-salvage-lanes/division`, `salvage/division` | C and linker integration tests |
| SDK IRQ state | DeepSeek / writer | RAM-visible libetc state and native IRQ callback binding, including required SDK consumers | `../psx-wasm-salvage-lanes/sdk-irq`, `salvage/sdk-irq` | SDK tests and build check |
| BIOS GPU | DeepSeek / writer | Added BIOS GPU entry points over the existing native GPU core | `../psx-wasm-salvage-lanes/bios-gpu`, `salvage/bios-gpu` | BIOS integration test and SDK checks |
| Library review | Qwen, low / read-only | Recovered BIOS libc/heap/callback correctness | recovery checkout, read-only pinned source | actionable findings only |

Parent owns guest-visible kernel chains/tables and subsequent runtime control
flow. The IRQ worker does not change BIOS kernel/runtime files; the GPU worker
does not change SDK IRQ state. Resolve cross-lane hunks only during integration.

Wave `8a735e9f-c16e-4ae8-acd0-09ae2839c736` is finished. All three writer
patches were source-reviewed and integrated. The Qwen libc reviewer timed out
at 1,800,000 ms while emitting its report; it made no tracked changes. Its
partial "no issues found" output is **not** an accepted review gate. Parent
review found and fixed a concrete `%n` ordering bug instead of rerunning it.

### Third delegation wave

Writers start from source checkpoint `1dc3509` in separate worktrees. No Qwen
run is planned for this wave. DeepSeek workers own:

| Lane | Boundary | Worktree / branch |
|---|---|---|
| Native files | Recover file/device dispatch and guest callbacks; preserve native CD/card operations, no controller driver | `../psx-wasm-salvage-lanes/files`, `salvage/files` |
| SDK pads | libapi/libpad RAM state and native kernel integration, no SIO protocol | `../psx-wasm-salvage-lanes/sdk-pad`, `salvage/sdk-pad` |
| BIOS oracle | Existing oracle BIOS backend and implemented-service vectors only | `../psx-wasm-salvage-lanes/oracle`, `salvage/oracle` |
| Unrelated dispositions | Read-only source-backed keep/rework/defer report for renderer/browser/fallback/residency deltas | recovery tree, pinned source |

Parent retains BIOS pointer thunks, ABI/linker/runtime control flow, executable
and exception work, and final review. Shared-file conflicts are integrated by
the parent, not by workers modifying one another's trees.

### Follow-up lanes and dispositions

Wave three and its files follow-up are complete and source-reviewed. Files
`cd7589c`/`fc93f94` are integrated as `1ed72a8`/`53a61b2`. The follow-up fixes
C(12h/13h/1Bh) dispatch, console guest-callback propagation, invalid redirects
and CD current-directory handling; its integration tests now assert callback
arguments and diagnostic output. Native card protocol/timing was not changed.

The isolated fallback lane recovered `7716465`, `c14c58d`, `28f4a57`,
`8736711`, `8a91a38` as parent commits `dc979a6`, `b989225`, `bf11e61`,
`71045f9`, `b0e9003`. Only shared CDC/GPU/timer/SPU corrections and necessary
BIOS/SDK call sites changed; no map, instruction timing or BIOS driver.

Unrelated source-backed dispositions:
- **Keep separately:** renderer error propagation `f664edd` -> `d1eb62d`.
- **Keep with small rework:** browser per-bundle disc storage `280b9e2` ->
  `4cce9e3`, including its message-shape documentation. Renderer tests and
  web/native compile checks passed; the changed disc path's browser gate is
  still pending. The existing card-storage browser test does not cover it.
- **Keep/rework:** the fallback batch above; integrated game gates below.
- **Defer:** overlay residency `474fb20` and prerequisite `4a92c4c`, pending a
  separate architecture decision; not needed for BIOS completion. Do not import
  archived ABI 13 or its phase-6 follow-ups.

## Current foundation validation (ABI 17)

Source checkpoint `4cce9e3`, in this recovery checkout, not main's outputs:

- `cargo test --workspace -- --nocapture`: pass after `2f1ac98` updates a stale
  lifter assertion: 150h is now the RAM-backed device-table pointer. BIOS 29,
  devices 63, SPU 9, SDK 36, runtime 7. Optional compiler test skipped without
  `PSXW_TEST_CC1`/`PSXW_TEST_CLANG`; two Docker oracle tests skipped without
  `.oracle/images.json`; threaded Docker/DWARF test remains explicitly ignored.
  Workspace run preceded the two isolated host patches, whose worker tests
  passed (renderer 9 and host-web/gpu-oracle checks).
- Release CLI and wasm32 runtime check: pass, rebuilt after integration.
- `census btn`: pass, including 8977/8977 layout entries. BOF4's default census
  could not find its matching cc1; `census bof4 --no-probe` passes, following
  the existing manifest's documented setting (not a newly weakened gate).
- `build btn` and `build bof4`: pass, all six chip groups native. Fresh ABI-17
  game/runtime outputs under this checkout's `out/`. Build's wording that all
  named BIOS calls are implemented is **not** a full-coverage certification.
- `run btn --replay title-to-gameplay --check no-trap`: 6000 frames, three
  expected states pass. `card-load`: 3000 frames, no trap; this alone is not a
  new save/persistence assertion. BTN logs a GPU timeout/reset around frame 64;
  not classified as introduced or resolved by these changes.
- `run bof4 --replay title-to-field --check no-trap`: 26000 frames, all five
  expected states pass. Supplied `craash-after-name-select`: 694-frame replay
  finishes without trap. No recordings or expectations changed.
- `conform btn --lib libpad`: 125 steps pass; `--lib libetc`: 60 steps pass,
  original translated SDK vs adaptors. Other SDK/chip/browser gates pending.
- `oracle check btn --device bios`: command prints PASS but compares **zero
  vectors** in the fresh store. Not accepted as oracle validation. Images and
  goldens still need provisioning; no capture/re-record performed.

### Full foundation CI classification (ABI 17 only)

The isolated `salvage/foundation-validation` checkout at `a74f832` ran both
commands to completion, each exiting **2**. Parent reviewed its handoff and
`out/ci/{btn,bof4}/ci.json`. This is evidence for source `4cce9e3`, not the later
ABI-18 implementation.

- Both: build, boot (11 checks), UBSan, SDK/chip conformance, SDK reach and
  native/all-device fallback gates pass; workspace release tests report 308
  passes, with optional compiler/Docker skips still applying. BOF4's configured
  replay passes (9 checks); its difftest/browser/mod gates skip by manifest.
- Both oracle gates fail on missing `.oracle/images.json`; BTN web-GPU also
  lacks vectors/goldens. These are absent prerequisites, not oracle passes.
- BTN difftest: 2620 pass, 3 excluded, **2 mismatches**: `DrawTextSlotImage`
  (`exe:80019E80`) and `Ovl28DrawSequenceGradientRect` (`ovl_28:800B8E9C`).
  A bounded diagnostic reproduced the first. Root cause/regression attribution
  is unknown: there is no comparable baseline gate result. Do not label these
  pre-existing source defects merely because this was the first full run.
- BTN mods reaches phase 3, then fails because previous-decomp census objects
  `out/census/btn/3a15992/obj` are absent. The suggested plain `census btn`
  command only builds the current pin, so its hint cannot provision that input.
- BTN Chromium and Firefox conformance, card storage, and title-to-gameplay
  (6000 frames plus recording replay) pass. Web-game imports the disc through
  this bundle's new per-game OPFS path and matches native RAM/VRAM/audio.
  This does **not** test two bundles sharing one origin or the locked-disc
  diagnostic; cross-bundle isolation remains unvalidated. Pacing is informational
  because the local machine classification is unset.
- Full and bounded-diagnostic reports/logs are retained in the validation tree
  and the `692606a4-f7a9-48cd-80a1-f62952fc09fb` subagent artifacts. No source,
  replay, expectation, user card or input snapshot was changed.

Bounded follow-up triage reproduced both mismatches at their original 64-seed
inputs. The packet allocator is declared `s32`; the call-out model therefore
returns scalar values, including -1. C folds the packet length into a word
store, while the lifted original stores that byte separately then reloads the
word. At pointer -1, those accesses straddle different masked-window aliases,
explaining the reported bytes. Parent reviewed the return model and disassembly
evidence. This does not establish a valid-pointer translator defect or baseline
regression attribution; the latter remains unknown. The full report was restored
byte-for-byte (SHA256 `513f26b56b8e2f87d5be16034622c28695cec41ae3e47c94a411d99267ee4b39`).
The suggested pointer-return annotation would change the input model; it is not
accepted as a domain-neutral fix. No exclusion, prototype/input-model edit or
new compiler heuristic was applied. Evidence is in workflow
`810c2b90-9237-4898-bca6-953b1a8efeff`'s `salvage/btn-difftest-triage.md` and logs.

Next: provision missing oracle/previous-decomp prerequisites and decide the
appropriate difftest input contract separately. Neither game CI nor full BIOS
completion is green.

## Current kernel-call batch (ABI 18)

- Source checkpoint `1513330`: callable kernel functions and callback ordering.
  A subsequent small batch restores C(00h) and C(0Ch) over those same native
  elements: counter IRQ masking/reset, enqueue order and autoack initialization.
  Its BIOS/runtime/SDK tests pass (30/8/36) and wasm32 check passes. C(01h)'s
  syscall handler remains pending a real native exception context; it is not
  guessed from IRQ state. Game artifacts/gates below are still at `1513330`.
- Native root-counter verifiers/handlers, IRQ-event delivery and pad/card
  sampling now have callable thunks at BFC08000h..BFC08030h (two reserved
  second-handler slots). RAM chain pointers are dispatched normally; the old
  element-identity bypass is removed, so guest replacements take effect.
- DeliverEvent callbacks run synchronously, including from a verifier or nested
  BIOS call, and see edits to later event blocks. Root acknowledgement/RFE
  follows the callbacks. IRQ events use the retail callback-then-autoack order
  documented by the retail variant in pinned OpenBIOS `handlers/irq.c`.
- Native RFE uses the existing saved BIOS thunk and wasm exception tag. No
  high-word return flag, new exception ABI, map or hardware driver was added.
  Nested guest-call depth is restored at the IRQ catch boundary.
- Workspace tests pass (BIOS 29, runtime 8, SDK 36, link-pass integration 6),
  including a patched-chain/callback regression and real clang/wasmtime calls
  through saved/constant/direct kernel pointers with nested RFE unwinding.
  Optional compiler/Docker skips and the ignored threaded-DWARF gate remain.
- wasm32 runtime check, release CLI and fresh builds of both games pass.
  ABI-18 BTN `title-to-gameplay` passes 6000 frames/all three expectations;
  BOF4 `title-to-field` passes 26000/all five. BTN's previously observed GPU
  timeout/reset is unchanged. Browser JS syntax checks pass.
- The isolated full-CI lane remains pinned to ABI 17 (`a74f832`); its results
  must not be attributed to this later batch. Card-load/crash replay, SDK
  conformance and browser gates have not been rerun at ABI 18 yet.
- C(06h), general non-local returns, callback thread switches, SDK BIOS-table
  replacement paths and other listed gaps remain unfinished. This batch does
  not complete the exception/control-flow checklist or full BIOS.

### Direct thread syscall follow-up (ABI 18 unchanged)

- SYS(03h) accepts an active TCB address and preserves its segment alias in the
  PCB, using the existing ChangeThread bookkeeping and runtime stacks. Invalid
  or closed TCBs fail explicitly; switching to self does not unwind.
- Both `psx.bios` and `psx.syscall` are Asyncify suspension imports. The build
  conservatively includes syscall sites because their selector may be dynamic.
  Rewind returns 1 at the suspended syscall without executing it again.
- BIOS 31 and runtime 8 tests pass; build crate compiles; wasm32 runtime check
  passes. All 7 link-pass tests pass, including a real clang/link/Binaryen/
  Wasmtime nested-call test that checks preserved locals and a single switch.
  The wasm test uses a host implementation of the suspension import; it is not
  a full-runtime thread oracle. No new game artifacts or replay results yet.
- Callback/IRQ switching and externally editable full TCB register contexts
  remain unfinished. No scheduler replacement, exception-context invention or
  ABI signature change was introduced.

### Next independent lanes

Workflow `2c933d53-de0b-4cad-8f92-79aeae391e51` starts two DeepSeek workers
from `1513330`, isolated in `../psx-wasm-salvage-lanes/kernel-services`
(`salvage/kernel-services`) and `../psx-wasm-salvage-lanes/native-cd`
(`salvage/native-cd`). The former owns only the listed kernel/configuration
APIs; the latter owns the missing BIOS CD APIs over existing native cores,
not the rejected register driver. Each must report disputed behaviour or a
required architecture change rather than invent it. Parent retains IRQ enqueue,
exception/non-local/thread control flow and integration. No subdelegation.
The separate ABI-17 foundation CI has completed; its failures and passing gates
are classified above, separately from these newer source changes.

Native-CD's `0aac185` plus corrective `827f56f` have been source-reviewed
and integrated with parent corrections. The original early wake, END-on-success,
and disconnected status/job state were not retained. A(54h/56h), A(78h/7Ch/7Eh/81h),
A(95h), and A(A4h..A6h) now use the existing native drive/controller and virtual
scheduler. Reads expose sectors as they complete, report final ACK then pause
DNE, and leave a missing sector's buffer unchanged with END. Shared core header,
subchannel, mode, head, EDC accounting, pending work, IRQ-line tracking and timing
remain coherent; bus-free reads also advance their drive head. There is no BIOS
register-command or DMA driver.

The parent separated A(95h)'s inner reset (success 1, existing events/cwd kept,
leaves critical) from `_96_init` (allocates five events, clears cwd). Boot installs
the events after its configured event table; SDK ResetCallback invokes the
patchable `_96_remove` before leaving critical. Partial event allocation rolls
back, and removal does not close slots reused for unrelated events. Per-operation
completion results prevent callbacks starting a new operation from changing a
blocking caller's result or consuming the new operation's event. A zero-time
seek still yields to the existing scheduler rather than recursing in its wait.

**Error-reference distinction:** psx-spx documents A(A5h/A6h) returning -1;
OpenBIOS at `4dc70d2931121a0fde858a0354192b6df4fa22e8`, `cdrom/helpers.c`,
`cdrom/cdrom.c` and `common/syscalls/syscalls.h`, calls A(A1h) SystemError first.
Terminal failures then return -1; a read ERR instead retries if the patch returns
(the initial integration missed this distinction, corrected below). The old
unconditional Rust panic has been replaced by the guest-patchable table call.
The existing default SystemError still stops execution; this is not an
unconditional nonfatal error policy.

Integrated tests: BIOS 36, devices 63, runtime 8 and SDK 36 plus wasm32 runtime
check pass. They use handcrafted sectors, include core/native handover, no-bus
head movement, multi-sector waits, zero-time seek, callback reentry, returning
SystemError patches and event cleanup. No fresh BIOS oracle comparison is claimed.

Fresh gates at integrated source `e2fb21b`, ABI 18: release CLI and both game
builds pass, all six chip groups native. BTN wasm is 2,500,066 bytes; BOF4 is
10,112,698 bytes. `title-to-gameplay` (6,000 frames, all three state expectations),
`card-load` (3,000 frames, no state expectations), `title-to-field` (26,000 frames,
all five state expectations) and the supplied name-select crash replay (694
frames) pass no-trap. Both games' loaded text matches disc and EDC error counts
are zero. BTN's GPU timeout at frame 64 and resets at 65/83 persist unchanged;
not claimed resolved or classified. These use fresh disposable run-card storage;
previous disposable storage was retained in the session scratchpad. They are not
a new persistence certification. BTN libetc conformance passes all 60 steps,
including original-vs-adaptor ResetCallback with installed BIOS CD events.
Reports/logs are retained in the session scratchpad as `cd-*`; this is not a
full fresh CI/browser/oracle gate, and earlier ABI-17 CI failures remain open.

**Still unfinished:** A(90h..93h), A(9Eh), A(A2h/A3h) callable handler services;
controller INT5 production (A94 currently exposes the zero initial bytes, not
complete error coverage); retail filesystem quirks/path-table state;
and exact blocking behaviour under guest-disabled/replaced driver events. The
current native wait retains an operation result rather than reproducing the
retail TestEvent polling loop. These remain real coverage gaps, not waived
"driver internals". No native-core/SDK ownership rewrite is authorised.

Kernel/configuration's `aa9cb54` plus corrective `fd9e662` have now been
source-reviewed together and integrated. SetConf clears chains before reinstalling
native defaults, including on repeated calls, and uses the parent's timer/IRQ
reset helpers. GetConf performs all three guest writes (address zero included);
C(08h) accepts its documented default range and intentionally invalidates old
allocations. SetMem, free-slot helpers and the five C0 zero-return entries work.
SetConf from a non-main thread, C(09h), GetSystemInfo identity indices 0/2 and
A(52h)'s reference conflict remain explicitly unfinished. Integrated BIOS 32,
runtime 8 and SDK 36 tests plus wasm32 runtime check pass. No runtime/thread
redesign, ABI change, game rebuild or fresh oracle comparison in this batch.

A further DeepSeek worker, `cde8d5ed-e38b-4833-bcb0-989b9570b1b3`, starts at
`22a4c87` in `../psx-wasm-salvage-lanes/card-completion`, branch
`salvage/card-completion`. It owns card waits, asynchronous file visibility,
busy/completion ordering, opt-in auto-format/reset and the listed card helpers.
It must use existing card images/scheduling, test only handcrafted disposable
in-memory cards, and ask before SDK scope or architectural expansion. No input
snapshots, real saves, game outputs, full CI or oracle captures in this lane.
Parent retains source review, integration and control-flow architecture.
Its initial `1dcd2bb` is **not accepted or integrated**: full review found
low-level callbacks redelivering their own low event, asynchronous FD-close/reuse
hazards, incomplete callback state transitions/error fields, and an extra read
completion vblank. Corrective run `15ef11e6-62fc-4b78-9fc6-442d57c0e63c` resumes
the same isolated worker with these findings, two disposable-card lifecycle
coverage, and public-reference verification of helper/channel semantics.
Its corrective `ffe6e60` is also **not integrated**. Parent read the full combined
patch and pinned references: B(58h) returns the requested device ID, not the last
slot index; generic close clears flags even on failure; the write-info stage
must not be consumed by the preceding frame's callback. Read completion orders
high then fd then low, while write-info orders fd then high then low in OpenBIOS
(the earlier parent brief incorrectly requested fd before high for reads).
Run `cf4f986e-fce2-4feb-a403-99d0b460f196` resumes the same worker with those exact
corrections, mid-operation error/new-card checks and auto-format event ordering.
Native close cancellation must protect a reused descriptor without changing the
generic close contract; callback coverage remains partial.

Final corrective `1510055` is now source-reviewed and integrated together with
its two predecessors. `_card_wait` uses existing Flow/vblank waits, async file
I/O moves one frame at completion, writes retain their separate info stage, and
new-card/removal failures preserve the partial offset without success events.
Generic close still clears flags unconditionally. Native close cancellation is
an explicit safety policy, not a claim about retail cancellation semantics;
it prevents the old open from modifying a reused fd (including other devices).
B(58h) returns the exact device ID. Parent removed a debug print, corrected the
close comment, preserved non-boolean auto-format values per `buSetAutoFormat`,
and strengthened the close/reopen test to cancel after one completed frame.
A(A7h..AAh/AEh) stay classified partial: native scheduling owns advancement,
Info/Load/write-test callback effects and exact return semantics are unresolved.
Integrated BIOS 47/runtime 8/SDK 36 tests and wasm32 runtime check pass. No user
cards, protocol driver, ABI change or new game artifacts in this batch.

After the user flagged unnecessary serialization, workflow
`810c2b90-9237-4898-bca6-953b1a8efeff` adds four parallel DeepSeek lanes alongside
that correction (no Qwen active; only Qwen has the one-at-a-time limit):

| Lane | Isolation and boundary |
|---|---|
| LoadTest/Load | `../psx-wasm-salvage-lanes/load-services`, `salvage/load-services`, base `e2fb21b`; file/device Flow only, no Exec/runtime/nonlocal redesign |
| Console helpers | `../psx-wasm-salvage-lanes/console-services`, `salvage/console-services`, base `e2fb21b`; circular buffers/input software/gets where existing seams suffice; missing host input and ioabort control flow require explicit blocker/decision |
| BIOS coverage reporting | `../psx-wasm-salvage-lanes/bios-coverage`, `salvage/bios-coverage`, base `e2fb21b`; distinguish working/partial/unsupported/documented fatal/noop, no new framework or runtime ABI |
| BTN difftest triage | Existing isolated `../psx-wasm-salvage-validation` at `a74f832`/ABI 17, source-read-only; bounded analysis of the two recorded mismatches, no exclusions/domain weakening or assumed baseline attribution |

Parent retains difficult runtime/control-flow work, source acceptance and
integration. No child may subdelegate, write input snapshots or real cards, or
expand architecture unilaterally.

Coverage worker `66b62cd` is integrated as `e005f10` with parent corrections:
`bios-calls.json` retains name/implemented and adds support; the summary no
longer claims all calls work. The existing build rejection policy is preserved.
Parent classified absent font data as unsupported, inherited CD/file/thread
limitations as partial, fixed alias names and tested a real saved-entry lookup.
Documented COP1-fatal conversions are named without fabricating conversions.
Integrated BIOS 38/build 1 tests pass. No new game artifacts were built; current
release CLI and game outputs remain at source `e2fb21b` until the next batch gate.

Load worker found the default kernel heap cannot hold its 0x800-byte header
sector. Parent authorized one reserved kernel data buffer at 8040h..8840h,
clear of existing data, native callable slots and kernel tables, using the
existing RAM-backed-data approach. No SP manipulation, heap expansion, runtime
interface or ABI-version change; static-buffer limitations must be reported.

Load worker `f23d6f5` is integrated as `2760dc9` with parent corrections. A(41h)
reads 800h bytes and copies exactly 60 bytes from offset 10h with no magic check;
A(42h) additionally reads the body, ignores its result, closes and flushes cache.
Parent fixed negative open/header-read comparisons and routed open/read/close
through the actual patchable B0 entries, rather than bypassing their table.
Tests cover a guest file device, default event count, exact copy, short/error
reads, returning patched calls and cleanup order. BIOS 41/runtime 8/build 1
plus wasm32 runtime check pass. Exec/LoadExec and warm boot are untouched;
normal boot still uses its existing loader. Static scratch is shared on reentry,
as in the reference. No game rebuild/replay or native-CD end-to-end loader
comparison is claimed for this batch.

A new isolated DeepSeek lane `f94473a0-cf46-47ee-b059-3acc641a0911` at
`../psx-wasm-salvage-lanes/sdk-bios-flow`, branch `salvage/sdk-bios-flow`, base
`2760dc9`, owns only libapi's init/start pad and firstfile BIOS-call continuations.
It must honor patched table calls and callback reentry using existing Flow/seq,
preserve the verified SDK helper's write/dequeue/enqueue ordering, and retain
its own operand addresses across nested adapters. Other SDK families and all
runtime/ABI architecture remain outside its contract; no subdelegation.

That lane's `9ea994e` is reviewed and integrated as `b121b7d`: libapi now uses
patchable dispatch/Flow, captures its original operands before callbacks, and
sets the init flag only after the BIOS call returns. Its two regression tests
cover patched ordering/reentry and firstfile's returned value. Parent also
converted libetc's InterruptCallback/ResetCallback and VSync timeout BIOS calls:
I_MASK restoration and subsequent setup/timeout continuations wait for guest
patches, while existing clock/IRQ behavior and original object addresses remain.
The combined SDK 39/runtime 8 tests and wasm32 runtime check pass. These are
source gates, not a fresh game/replay or original-SDK conformance result.

Resume `aca5bd30-9fcd-4232-a592-8ed276a12afc` completed libpad's five analogous
PadStartCom/PadStopCom calls as `2a18cb5`, reviewed and integrated as `02f2a66`.
Parent verified `pdmaiini.o` and restored the missing EnterCriticalSection calls
before the patched BIOS work (the matching exits already existed), and changed
the regression to start outside critical rather than hide that omission. SDK 40/
runtime 8 tests and wasm32 runtime check pass. Libmc's synchronous BIOS bypasses
(including its void IRQ-step path) and libgpu's direct GPU_cw call remain partial;
no global helper or IRQ/runtime redesign is authorized.

Workspace tests passed at `6deb05b` (card/libapi/libetc batch): BIOS 47, runtime 8,
SDK 39, linker integration 7, and other crates. Optional compiler-env tests and
two Docker oracle cases still skip for missing prerequisites; threaded Docker
DWARF stays ignored. The release CLI also rebuilt there; game/runtime wasm
artifacts still require the next combined build/replay gate.
Read-only DeepSeek run `008e4a9a-fd2e-4f8a-9117-34759266ba0f` investigates only
A(52h)/C(09h) evidence at parent `55d2c2e`, with a four-reference bound and no
implementation authority. Console research continued broad searches after two
steers. Parent interrupted `f8f7657d` on its third watchdog notice, verified no
tracked changes at `e2fb21b`, an empty report and the stopped search processes.
The original four-lane workflow is complete with that child marked failed/paused,
not a successful console implementation. One same-channel retry `5f2da315-0130-
4b17-a89f-c9decd7845db` now owns only gets using the supplied pinned stdio body;
circular-buffer services, missing host input and general ioabort remain blocked.
No alternate runner or tool/config workaround was used.

The gets-only retry completed as `26264cc`, reviewed and integrated as `cec5896`.
Its iterative immediate-return path and existing Flow continuations preserve
editing, quoting, the 125-byte limit and read/echo order over guest devices.
Parent fixed the final NUL write to occur AFTER the newline echo returns, as in
pinned stdio.c, with a callback-time assertion, and classified gets as partial
rather than unsupported. BIOS 48/runtime 8 and wasm32 runtime check pass. The
built-in host-input error remains; C(15h..19h)/A(B2h) are not implemented.

The kernel-entry scout's report is reviewed, but its proposed A(52h) answer is
NOT accepted: a PsyQ GetSysSp tail-jump proves the SDK binding, not the retail
body, side effects or whether the return is stack-top/top-minus-four/live SP.
No guessed 80006FFCh return or fatal classification was added. C(09h)'s documented
post-boot problem is its FIRST OPCODE being overwritten by the A0 table's last
word, not a known callable operation that corrupts data. Literal clearing of
7460h..891Fh would destroy unrelated native file data while not resetting Rust
kernel state; no clear/reset implementation is justified. Both stay unfinished.

### Card/SDK integration gate at `37e37ed` (ABI 18)

Fresh release CLI, runtime wasm and both game builds pass, all six chip groups
native. BTN is 2,544,749 bytes; BOF4 is 10,157,727 bytes. The new build report
actually prints BTN 42 working/12 partial/1 unsupported/1 fatal/1 no-op calls,
and BOF4 36 working/16 partial/1 unsupported/1 no-op; unsupported entries are
B(51h) font access and A(43h) Exec respectively, not build failures under the
preserved named-entry gate.

Fresh disposable storage and unchanged replays: BTN title-to-gameplay (6000
frames, all three expected states), card-load (3000 frames), BOF4 title-to-field
(26000 frames, all five expected states), and supplied name-select crash (694
frames) all pass no-trap. Boot text matches and EDC errors are zero. Card-load
has no state assertions and is not save/persistence certification. BTN GPU
warnings at frames 64/65/83 remain unchanged and unclassified. Original versus
adaptor conformance passes for libpad (125 steps) and libetc (60 steps).

At this checkpoint artifacts were at `37e37ed`; the later gets source was only
locally gated. No fresh full CI, browser or non-empty BIOS oracle result was claimed. ABI-17 CI
failures/prerequisite gaps remain recorded above. New SDK continuation work is
isolated in `5941fc75-3407-47dd-887d-3b601c2ea103`, same SDK lane: ONLY libmc's
start_com and finish (three direct BIOS calls). That lane has completed as
`b88bd21`, full-source reviewed and integrated as `61a8c11`. Parent checked the
actual mcxinit.o and restored McxStartCom's Enter/ExitCriticalSection calls,
including Exit's existing pending-IRQ queue behavior, without changing finish's
IRQ context. The test now starts outside critical. Parent also routed libgpu's
ResetGraph GPU_cw through patched dispatch, preserving its original argument and
existing error handling; GPU reset/GEnv writes wait for that call to return.
SDK 41/runtime 8 and wasm32 runtime check pass. These changes were initially
locally gated; the later combined `d06c81b` gate below refreshes the artifacts.

The four remaining libmcx IRQ-path BIOS bypasses are now converted by the
parent. Read-only scout `a122da85-3103-4a9a-9839-7e88fdaf259a` identified the
existing static member/offset binding and ordinary Chain/Flow seam. Parent
verified `mcxresi.o`'s actual handler and BIOS/DSR call ordering. The handler
is bound as `libmcx/mcxresi:0x80`, under the existing member identity hash;
STEP_CARD queues it as an ordinary guest call in exception context, like
STEP_DSR. It locates its data through the handler's first relocated load of
`_mcxCmdSeq`, not by treating its text address as data.

Dequeue/enqueue calls now use patchable dispatch and `machine::seq`; retry and
timeout state changes retain their places around those calls. The DSR call
and its subsequent IRQ7 acknowledgement/unmask are one Flow continuation,
replacing the old separate tail step. A nested RFE unwinds to the existing
outer IRQ call, skipping the continuation. Captured library operands survive
SDK reentry, and retry status is reread after the patched dequeue. The now
unused synchronous SDK BIOS helper is removed. No trait, runtime, ABI, shared
IRQ, wire or clock changes are introduced.

Focused gate: SDK 42/runtime 8 tests and wasm32 runtime check pass. Existing
slot/card tests now traverse the queued handler/DSR path; one grouped test
checks reserved/retry/timeout order, patched services without native duplicate
execution, reentry, retry cancellation and RFE skipping. The existing memcard
conformance cases exercise this handler on every transfer and now name it in
their coverage. The conversion is committed as `d06c81b`.

### SDK Flow integration gate at `d06c81b` (ABI 18)

Release CLI/runtime and both game builds pass with all six chip groups native.
BTN is 2,568,958 bytes; BOF4 is 10,182,849 bytes. BTN's static mcxresi+80h handler
is actually bound as an adapter in `tiers.json` and the build adapter list;
BOF4 does not link libmcx. Coverage totals and unsupported entries are unchanged
from the prior gate; no ABI or supported-service inflation is claimed.

Conformance passes: memcard native versus device (9 case runs: start and command
at four seeds each, plus a full session) and libgpu original versus adaptor
(134 steps). Normal `game.wasm` remains native; comparison artifacts are separate.
All four unchanged replays pass again: BTN title 6000 frames/all three expected
states and card-load 3000, BOF4 title-to-field 26000/all five expected states and
the supplied crash replay 694. Boot text matches, EDC errors are zero; BTN's
64/65/83 warnings remain unchanged and unclassified. Card-load and the supplied
crash replay have no state assertions; these are not persistence certification.
Only disposable run storage was used; previous disposable files were preserved
in the session scratchpad. Source tests/build/conform/replay evidence is in
`mcx-irq-integrated-tests.log` and `mcx-flow-*` scratch reports.

Independent read-only review `e9e68d0c-bafa-4e72-8dea-50b9a9ba34bf` is complete;
parent read its full report and accepted the conversion with no introduced
correctness defect found. Its required real-binding gate is satisfied by the
fresh build reports and memcard conformance above. Parent rejected its claimed
inherited retry-exhaustion status mismatch: mcxresi's branch delay slot at CCh
loads 11h before the status store, matching the adapter (not retry count 3).
The focused RFE test uses a test double in patched dequeue; no new real-wasm
MCX/DSR RFE test is claimed. Existing nested-RFE machinery tests cover the unwind
mechanism, and no transport or timing change was requested or made.
No new full CI, browser or non-empty oracle result is claimed. The documented
ABI-17 CI failures and missing prerequisites are unresolved. General non-local
return, executable/exception context, fonts/identity policy and other partial
BIOS services remain open; this is not full-BIOS completion.

## Deferred bug: general non-local return targets the wrong call stack

Parent traced the remaining non-local return gap through `psxw-lift/src/lib.rs`
and `emit.rs`, `psxw-link/src/game.rs`, and `psx-runtime/src/wasm.rs`/`driver.rs`:

- Recognised A(13h) sites already save RA/SP/FP/S0–S7/GP and produce a compiled
  continuation at the return address. Entering that continuation as an ordinary
  guest call does **not** discard the active calls between longjmp and setjmp;
  its eventual return would go back to the longjmp caller, not the saved caller.
- The existing wasm RFE tag unwinds only to `guest_invoke_irq`. It is not a
  general jump-to-live-frame mechanism. Indirect/unrecognised setjmp calls also
  lack the caller register capture that the known-site lowering supplies.
- Asyncify preserves live wasm locals for thread switches, but its current
  scope excludes exception-handler functions. Adding catches to guest functions
  cannot simply be assumed compatible with that existing thread machinery.
- Native `Regs` exposes SP/GP only. Exec's caller context, general non-local
  returns and editable exception contexts cannot be completed by fabricating
  FP/RA or merely calling a compiled entry point. Callback/IRQ thread switching
  remains separately blocked; replacing Asyncify is not authorised.

**User disposition:** note the bug and continue the independent plan work;
revisit this later. No approval request is pending for this item, and neither
a new control-flow design nor its implementation is part of the current work.
When resumed, establish target ownership across recursion/threads, register
capture and correct frame cleanup/return before proposing a concrete change.
Retain Asyncify; no interpreter, simulated CPU register file or scheduler
replacement. A significant architecture change still requires review before code.

## Console/card contract follow-up

Read-only workflow `93e1e227-2f2c-49d5-8c86-7e755960d251` completed both bounded
contract scouts; parent read both reports in full and checked the relevant
pinned OpenBIOS bodies. No children remain active. Reports are
`salvage/console-circular-contract.md` and `salvage/card-callback-contract.md`
under that workflow's session artifacts. The first launch was rejected before
starting children because the parallel shorthand disallowed per-child output
paths; one documented scripted-workflow retry succeeded. Neither scout edited
source or had implementation authority.

Parent implementation in `psx-bios/src/{files,bios,lib,card}.rs`:

- **C16 `_cdevscan`:** OpenBIOS `fileio/misc.c` walks all 16 FCBs, tests flag
  1000h, then calls `psxioctl(fd,6602h,0)`. Native implementation uses existing
  `ioctl`/`Flow`/`seq`; reads each flag after preceding callbacks, ignores ioctl
  failure results while preserving errno, and neither snapshots nor invents
  input. `fileio/stdio.c` also scans before read/write: invalid fd fails first,
  then device/transfer fields are read only after scans return. The old no-scan
  path's immediate errors remain errors. Callback-continuation errors retain
  the existing file-flow diagnostic policy. C16's void/unspecified v0 uses zero;
  this is not an input-availability result. Coverage is `Partial` because the
  built-in host-input path is still missing. Contrary to the scout's no-op
  suggestion, guest-provided scan drivers are useful without a built-in host ABI.
- **C15/C17/C18 remain blocked:** the pinned OpenBIOS table points them at
  `unimplementedThunk`; the circular-buffer header and DTL-H2000 TTY consumer
  do not establish retail full/empty/flags/wrap/error semantics. No guessed ring
  buffer. C19/A(B2h) still depend on the now user-deferred non-local-return bug.
- **A8/A9/AA, pending Info/Load:** `card/backupunit.c` error callbacks clear
  `g_buOperation` and emit high 8000h/100h/2000h only for an active operation;
  they do not change low-driver status or emit its event. Existing queued native
  Info/Load operations now clear only their high-notification flag. The correct
  last-completed slot is used, repeat/idle/low-only callbacks do nothing, and
  the existing scheduler still completes the low operation. Load gains the
  same high flag as Info. File cancellation policy and A7 are unchanged.
- **Narrow acceptance, not the whole card proposal:** the scout also proposed
  extending this to write-test and AE; those bodies/effects are not established
  sufficiently, so parent did not extend them. No inferred failed-write image
  policy or new per-slot state framework. The existing callback v0 remains
  explicitly unspecified. All five callbacks remain `Partial`; native Load's
  existing simplified directory behavior is not claimed retail-complete.

Reference pin for all three bodies:
`pcsx-redux/nugget@4dc70d2931121a0fde858a0354192b6df4fa22e8/openbios/`.
One grouped guest-device integration test covers direct C16, live flag changes,
last descriptor, negative ioctl, both read/write device paths and post-scan
field lookup. One grouped card test covers A8/A9/AA × Info/Load, both slots,
A7 non-advancement, repeat/idle/low-only guards, unchanged busy status and no
second high completion. The first scan gate found an obsolete assertion that
C16 must error; replaced with the still-missing C15/C17/C18 assertions, not a
weakened scan test. Final **BIOS 50 / runtime 8 / SDK 42 tests pass**, plus the
wasm32 runtime check. Log: `console-card-callback-integrated-tests.log` in the
session scratchpad. Formatter-only changes were removed before integration.
No runtime ABI, scheduler, register driver or host API change.

At initial integration, release/game/conformance/replay artifacts remained at
**d06c81b**; the follow-up gate below now refreshes them for `99fa333`.

### Console/card integration gate at `99fa333` (ABI 18)

Fresh release CLI, runtime wasm and both game builds pass, with all six chip
groups native. BTN is 2,575,785 bytes; BOF4 is 10,189,724 bytes. BIOS support
counts are unchanged (BTN 42 working/12 partial/1 unsupported/1 fatal/1 no-op;
BOF4 36 working/16 partial/1 unsupported/1 no-op). Asyncify scope is unchanged,
including the single excluded exception-handler function in each game.

All four unchanged no-trap replays pass: BTN title 6000 frames/all three expected
states and card-load 3000; BOF4 title-to-field 26000/all five expected states and
the supplied crash replay 694. Boot text matches and EDC errors are zero. BTN's
GPU warnings at 64/65/83 remain unchanged and unclassified. Card-load and the
crash replay still have no state assertions. Previous disposable run storage
was moved to the session scratchpad; fresh disposable storage was used, not
user saves. Logs/reports: `console-card-{release,build-*,replay-*}` in scratch.

Memcard native-versus-device conformance passes all nine case runs, including
four seeds each for start/command and the session case. Log:
`console-card-conform-memcard.log`; report: `out/conform/btn/chip-memcard.json`.
Comparison modules remain separate from normal native game artifacts. The
already-passing BIOS 50/runtime 8/SDK 42 and wasm32 checks cover this source;
no unnecessary workspace rerun was performed. Full CI, browser and non-empty
oracle results are not refreshed, and their recorded gaps remain open.

### Documentation and CD contract review after the deferral

Both reports from workflow `1610e372-e898-4cf0-946a-0dc7f60bd941` were read in
full: `salvage/docs-reconciliation.md` and `salvage/cd-wait-contract.md` in the
session artifacts. Documentation commit `05024b3` was reviewed and integrated
as `124d922`. Parent corrected the current libmcx Flow description and removed
an ambiguous suggestion that the recovered native chain pointers still lack
callable implementations. The plan index now identifies this active recovery;
`plans/open-items.md` records current BIOS gaps, actual CI failures, the deferred
non-local bug and the historical status of its old audit. No new architecture
was accepted. A progress steer arrived after the documentation lane had already
completed; it changed no work, and fresh validation facts were integrated here.

CD review confirms the pinned OpenBIOS wait priority: seek/status test DNE before
ERR; reads test DNE, ERR, then END. At that checkpoint, per-operation completion
Cells did not fully reproduce that guest-event contract (the later live-wait
batch below replaces that selection, retaining captured status/cancellation).
**The scout's proposed completion-only selector is not accepted:** falling back to the Cell when no event is ready
would still ignore disabled/consumed events, and it would not return when an
early guest event becomes ready. Its proposed read-ERR test also assumes that
SystemError(44h,16h) returns -1 immediately; the reference instead continues its
retry loop if the SystemError patch returns. Test events injected before seek
completion can be cleared by the next read's `cd_begin`, so phase matters.

Parent read pinned `openbios/cdrom/helpers.c` and `cdrom/cdrom.c` directly.
`helpers.c` also explicitly changes retail's sector-mangling retry bug; it is
not unqualified retail parity evidence. The existing operation identity protects
callback reentry and must not be discarded. Next CD work must handle event wake,
consumption, cancellation/reentry and returning-error retry behavior coherently
using the native software/scheduler seams; merely changing late error priority
would be a hybrid contract. No CD code or tests were changed on this evidence,
and no new architecture approval request is being made. These services remain
partial, independently of the user-deferred non-local-return bug.

### CD-directory and search software follow-ups

Parent commit `7422359` corrected A(64h) `dev_cd_chdir`, also reached through B(40h) `cd`, using
psx-spx's file-function contract and pinned OpenBIOS
[`filesystem.c`](https://github.com/pcsx-redux/nugget/blob/4dc70d2931121a0fde858a0354192b6df4fa22e8/openbios/cdrom/filesystem.c).
The old native implementation incorrectly required the directory to exist and
appended a relative new name to the old current directory. The call now replaces
the upper-cased current directory before reading metadata, accepts nonexistent
directories, and returns the result of reading the PVD signature and its first
little-endian path-table sector through the existing host disc. A failed read
still keeps the new directory; an empty name resets it to root. The existing
native slash normalization remains. No hardware-driver, timing, scheduler or ABI
change was introduced.

The existing device-table integration test now checks nonexistent-directory
acceptance followed by failed relative open, replacement by another relative
name, root reset and unchanged errno. One grouped failure test covers absent
PVD, invalid signature and absent path-table sector through both B40 and A64;
the name persists and the generic temporary FCB is released. **BIOS 51, runtime
8 and SDK 42 tests plus wasm32 runtime check pass**, logged in the session
scratchpad as `cd-chdir-integrated-tests.log`; the complete code diff was reviewed.

A64/B40 remain `Partial`: native lookups reread directory data rather than
retaining Sony's path-table/disc-identity cache. Other CD filesystem quirks and
the separately documented event-wait/retry contract are unchanged. This does
not resolve the rejected wait proposal. Release/game/replay/conformance outputs
remain at `99fa333` until a later integrated refresh; no new full CI, browser or
oracle pass is claimed.

The next small follow-up corrects the CD `firstfile`/`nextfile` name matcher,
using the same pinned reference's `dev_cd_firstFile` and `patternMatches`:
`*` ignores all following pattern text rather than implementing a shell glob;
`?` matching the name terminator also accepts the remaining pattern (the
documented BIOS bug). Searches compare the full ISO name, including `;1`,
rather than stripping its version. File-open's existing optional-version lookup
is unchanged. The shared native disc core and card matcher are not modified.
The existing device-table test covers first/next search order, lower-case input,
star suffix rejection avoidance, the question-mark terminator case and explicit
matching/mismatching/omitted versions. BIOS 51/runtime 8/SDK 42 plus wasm32 check
pass (`cd-search-integrated-tests.log`). A62/A63 and B42/B43 remain `Partial`;
this does not implement Sony's directory cache, disk-change quirks or arbitrary
invalid/overlong filename behavior.

### Fresh CD-filesystem integration gate (`bbdeb39`, ABI 18)

Release CLI/runtime and both game builds pass. All six chip groups remain
native. BTN is 2,572,791 bytes (86 adapters); BOF4 is 10,186,726 bytes (91).
Coverage totals are unchanged: BTN 42 working/12 partial/1 unsupported/1 fatal/
1 no-op; BOF4 36 working/16 partial/1 unsupported/1 no-op. Asyncify remains
1296/7158 and 3009/18393, with one exception handler excluded in each.

All four unchanged replays pass with explicit frame limits and `--check no-trap`:
BTN title-to-gameplay 6000 (all three expectations), card-load 3000 (no state
assertions), BOF4 title-to-field 26000 (all five expectations), and the supplied
name-select crash replay 694 (no state assertions). Boot text matches and EDC
errors are zero in all four. BTN's frame-64 timeout and frame-65/83 resets remain
unchanged and unclassified. Fresh disposable run storage was used; previous
run storage is preserved in session scratch, not user saves.

Logs/reports are session scratch `cd-files-release.log`, `cd-files-build-*.log`
and `cd-files-replay-*.{log,json}`. Older scratch logs from the preceding session
checkpoint are no longer present there; their committed plan records remain
historical evidence. No unnecessary memcard/SDK conformance, workspace or full
CI rerun was performed for these two CD filesystem changes. Latest memcard
conformance remains at `99fa333`, GPU at `d06c81b`, libpad/libetc at `37e37ed`.
At this checkpoint browser, non-empty oracle and save/reload-persistence gaps
remained open. The focused browser storage follow-up below is now complete.

### Browser disc isolation and locked-import gate (`8e5badf`)

Worker `e18f62e` was fully reviewed and integrated as `12c5fbb`. The test packages
two production pages with different `game.json` ids on one origin, using the
same cue/bin names but different synthetic bytes. A worker opens the production
`Disc` and holds its real `SyncAccessHandle` while a page attempts to import a
different replacement image. Both browsers verify the running-game diagnostic,
unchanged cue/bin/key, unchanged worker-read sector, successful replacement
once unlocked, and no changes to the other bundle. All seven existing card
checks remain. The worker's negative sensitivity report, reviewed by the parent,
fails specifically the cue-preservation check with the earlier sequential-write
fix; the replacement check is not vacuous.

The production fix acquires all incoming writable streams before copying any
file and aborts acquired streams on lock refusal. Old metadata and stale files
are not removed until copying succeeds. Parent `8e5badf` extends cleanup to copy
failures too: not-yet-used writers must not keep the disc locked. A focused
injected read failure through the real page's file input verifies its diagnostic
and subsequent exclusive `Disc.open` with the original bytes intact. This is
resource cleanup, **not transactional import**: a non-lock failure after some
files have committed can still leave a partly replaced image.

Parent release CLI build and
`psxw web-test storage --browsers chromium,firefox --timeout 180` pass:
**25 checks in Chromium and 25 in Firefox**. Both JSON reports were read under
`out/web/test-storage-results/result-{chromium,firefox}.json`; session scratch
logs are `browser-disc-integrated-{build,storage}.log`. The existing pinned
browser image was reused. Synthetic disc/card bytes and browser profiles are
disposable; no game inputs or real saves were changed.

No runtime/ABI source changed, so native builds/replays remain valid at
`bbdeb39`; no redundant native gates ran. This storage test does not start the
full game worker/wasm or certify game save/reload, GPU/browser conformance,
non-empty BIOS oracle comparisons or green full CI. Those separately recorded
gaps and the user-deferred non-local-return bug remain unchanged.

### ABI-18 browser conformance and game gate (`9624526`)

The existing CLI gates ran against native source `bbdeb39`, browser source
`8e5badf`, and fresh ABI-18 bundles at documentation checkpoint `9624526`:

- `web-test conform btn --browsers chromium,firefox --timeout 600`: both
  browsers pass libpad (846 steps), libgpu (263), libmcrd (105), libetc (122)
  and libspu (14). All five result records are present in each JSON, with no
  diffs and matching native final VRAM/audio. The existing command omits
  scenarios needing probes; this is not all SDK/chip conformance.
- `web-test game btn --replay title-to-gameplay --frames 6000 --hash
  300,1200,2400,3600,4800,6000 --record --browsers chromium,firefox --timeout
  1800`: both browsers and native reach 6,000 frames; all six RAM/VRAM samples
  and all 4,423,466 audio frames match. Both pages display lit frames, import
  the read-only disc into OPFS and execute the real wasm. Browser recordings
  replayed natively reproduce the same sampled RAM. No replay expectations
  changed, no EDC errors or guest I/O accesses reported.

Parent read both logs and all four result JSONs. Reports are in
`out/web/test-conform-btn-results/` and `out/web/test-btn-results/`; logs are
`browser-abi18-{conform,game}.log` in session scratch. Previous own disposable
BTN storage was preserved there as
`browser-abi18-btn-previous-disposable-storage`; tests used fresh own storage
and disposable browser profiles, not user cards or writable input snapshots.

The pinned browser image was reused. These gates refresh the broader BTN
browser coverage beyond ABI 17, but do not provide missing GPU-oracle goldens,
BTN mod inputs, BOF4 browser coverage, save/reload certification or green full
CI. No source/runtime/ABI changes were needed for this validation batch.

### A(40h) fatal service and BREAK contract review

After the browser gate, parent implemented the independently specified
A(40h) `SystemErrorUnresolvedException`: its default entry uses the same fatal
BIOS error path as A(A1h), rather than returning or spinning the host. Coverage
now names/classifies it as `Fatal`, not missing/`Unsupported`. The existing
BIOS table integration test checks the native error, guest-patched call/result,
and saved native pointer remaining fatal. **BIOS 51 / runtime 8 tests and
wasm32 runtime check pass** (`a40-integrated-tests.log` in session scratch).
No runtime import, ABI, event scheduler or compiler change. Existing builds and
browser/game gates remain at their stated pre-A40 source checkpoints; no fresh
full build/CI or BREAK integration test is claimed for this small service change.

Read-only DeepSeek scout `6c29956f-b4a7-44ce-9e51-61b6a5c3ba62` completed;
parent read its full `salvage/break-contract.md` report and independently checked
[psx-spx's BIOS contract](https://psx-spx.consoledev.net/kernelbios/), runtime
flow/exception handling, and linker division helpers. BREAK must deliver
`F0000010h/1000h` then call A40, not the unknown-syscall event `4000h`.
The proposed plain `deliver_event_now`/`driver::run` composition does not
establish exception context, RFE unwinding or custom-exit/return behavior, so
it was not implemented. BREAK remains count-only and visibly unfinished.

Parent **rejected** the report's claimed need to distinguish compiler division
checks from explicit BREAK: those helpers deliberately reproduce the original
GCC BREAK instructions. The differential harness's returning callout does not
establish a separate production exception policy. No origin discriminator,
new import, or speculative patched-return termination policy was added.
The general longjmp/ioabort deferral is unchanged; this review neither designs
that work nor declares all exception work dependent on it. No child remains active.

### BIOS CD-open filename follow-up

Parent checked [psx-spx's file contract](https://psx-spx.consoledev.net/kernelbios/#bios-file-functions)
and the complete pinned OpenBIOS
[`filesystem.c`](https://raw.githubusercontent.com/pcsx-redux/nugget/4dc70d2931121a0fde858a0354192b6df4fa22e8/openbios/cdrom/filesystem.c).
`dev_cd_open` uppercases the filename and appends `;1` only when no version
was supplied. Its filename matching accepts `?`, including the documented
name-terminator bug, but does not expand `*` or wildcard directory names.

The BIOS previously used the shared disc lookup, which strips every version
suffix and has no wildcard matching. The correction uses the existing native
BIOS directory traversal and matcher instead, with star expansion allowed only
for directory searches. Shared `cd::find`, SDK/native disc lookups, CdGetLbn,
card matching, scheduling and the ABI are unchanged. Public open and A5F remain
`Partial`: this does not implement path-table caching/disc changes, alignment,
or invalid/overlong-name quirks, and does not claim a full CD filesystem.

The existing device-table integration test now covers case normalization,
implicit `;1`, a distinct existing `;2`, nonexistent/empty versions, `?` in
names/versions, first-match order and the NUL quirk, rejection of star/directory
wildcards, FCB metadata and failure cleanup. Existing search tests remain,
including enumeration of both file versions. **BIOS 51 / runtime 8 / SDK 42
and wasm32 runtime check pass**; the final versioned fixture also passes its
focused integration rerun (`cd-open-integrated-tests.log` in session scratch).
No input assets or user storage were changed.

### CD-open integration gate (`03c4627`, ABI 18)

Fresh release CLI, runtime and both normal game builds pass, including the A40
service from `6ab48a0`. All six chip groups remain native; BTN has 86 adapters
and a 2,578,555-byte module, BOF4 91 and 10,192,498 bytes. Asyncify counts remain
1,296/7,158 and 3,009/18,393, with one exception handler excluded in each build.
Named BIOS coverage counts are unchanged (BTN 42 working/12 partial/1
unsupported/1 fatal/1 noop; BOF4 36 working/16 partial/1 unsupported/1 noop).

All four unchanged replays pass with explicit frame limits and `--check no-trap`:

- BTN `title-to-gameplay`: 6,000 frames and all three state expectations.
- BTN `card-load`: 3,000 frames; no state assertions, not save certification.
- BOF4 `title-to-field`: 26,000 frames and all five state expectations.
- BOF4 supplied `craash-after-name-select.rpl`: 694 frames; no state assertions.

All boot text matches the disc; EDC errors are empty and guest I/O reports are
empty. BTN's frame-64 GPU timeout and frame-65/83 resets are unchanged and still
unclassified. Logs and copied run JSON are in session scratch under
`cd-open-{release,build-*,replay-*}`. Previous own disposable storage was preserved
as `cd-open-{btn,bof4}-previous-disposable-storage` before fresh run storage was
created; user cards, input snapshots and original main were untouched.

Browser storage/conformance/game and memcard/GPU/SDK conformance keep their
previous recorded source checkpoints. No unnecessary full workspace/full CI,
empty oracle or browser rerun; missing oracle/mod inputs and save/reload
certification remain open.

### CD chain-control contract scout disposition

Read-only DeepSeek scout `3a8b5d32-7afa-4e98-8ada-bf0af141b47b` completed
`salvage/cd-internal-chain-contract.md`; parent read the full report and checked
psx-spx plus pinned OpenBIOS `cdrom.c` and
[`statemachine.c`](https://raw.githubusercontent.com/pcsx-redux/nugget/4dc70d2931121a0fde858a0354192b6df4fa22e8/openbios/cdrom/statemachine.c).
The reference has separate IO/DMA flags (A9E returns the old flag, inner init
sets both to 1), consumed by handlers that acknowledge their shared IRQ and
return from exception. A2 installs IO then DMA at priority 0; A3 dequeues IO
then DMA. OpenBIOS declares those two functions void; psx-spx supplies no
precise return contract and records retail's dequeue bug.

Parent **rejected** the recommended flag/chain-plumbing-only implementation:
unused flags and enqueuing pointers to still-unhandled A90..A93 would add no
working completion path and could make existing interrupts fail. The present
native CD scheduler delivers software completions directly, not through those
handlers. Completion needs a coherent native completion/chain/event/auto-abort
contract using existing facilities, not a reconstructed CD register driver.
The report's claim that any such routing necessarily requires a register driver
is not accepted. Its double-enqueue self-link claim is also imprecise: the shown
two-element sequence produces a cycle; its final reverse-order dequeue advice
contradicts the quoted IO-then-DMA body. The reported A93 dispatch-table anomaly
must not be silently copied. No chain state, flags, ABI, runtime or exception
machinery was added from this scout, and no new architecture was approved.
No active children or pending worker changes remain.

### CD read retry follow-up (`c42c64c`)

Parent corrected the returning-error continuation in `cdrom.rs`, using the
existing `Flow::Call`/`machine::seq` machinery only. Pinned OpenBIOS
[`helpers.c`](https://raw.githubusercontent.com/pcsx-redux/nugget/4dc70d2931121a0fde858a0354192b6df4fa22e8/openbios/cdrom/helpers.c)
shows that read ERR calls A(A1h) with `(44h,16h)` and retries if that patch
returns, up to ten seek/read attempts. Exhaustion calls `(44h,0Ch)` and returns
-1 if it returns. Seek errors, rejected commands and END remain terminal.
The original destination/count survive the callback, and each later error
looks up the current A1 table entry. Selected ERR is consumed before the error
callback when it belongs to that operation, as for existing DNE/END handling.

The reference explicitly documents that retail computes MSF inside the retry
loop and mutates `sector %= 4500`, losing its minute on retries; OpenBIOS moves
that computation outside the loop to fix it. The native retry keeps the reported
retail behavior rather than silently copying that OpenBIOS divergence. This is
software argument handling, not command/instruction timing.

One grouped integration test exercises a partial two-sector transfer interrupted
through A95, software-delivered ERR, a returning error patch, successful retry,
and ten-attempt exhaustion. It checks the buffer/count, lost-minute behavior,
ERR consumption, no retry before the callback returns, live table replacement
and no eleventh attempt. The existing error test now also covers terminal
seek/status errors. Tests use disposable handcrafted sectors, not user discs.

**Gate:** BIOS 52/runtime 8/SDK 42 tests and wasm32 runtime check pass
(`cd-retry-integrated-tests.log` in session scratch). A5/A6 remain `Partial`:
at this retry checkpoint, waits still used per-operation results rather than
live TestEvent polling (superseded by the live-wait batch below);
command-busy retry loops and native INT5 production are not implemented. No
Wait variant, event-generation state, IRQ/CD driver, ABI or runtime redesign.
Latest release/runtime/game builds and four replays remain at `03c4627`, before
this correction; no unnecessary full-CI/browser/empty-oracle rerun.

### Card write-test contract scout disposition

Read-only DeepSeek scout `ce3b09e8-84df-43f2-9bfe-f7f50cfc95f5` completed
`salvage/card-write-test-contract.md`. Parent read its complete report and
independently checked pinned OpenBIOS
[`handlers.c`](https://raw.githubusercontent.com/pcsx-redux/nugget/4dc70d2931121a0fde858a0354192b6df4fa22e8/openbios/kernel/handlers.c):
A(AFh), like AE, is unimplemented. psx-spx documents the dummy frame-3Fh write
and new-card flag clearing, but not its high-level notification contract.

Parent **rejected** the scout's proposed `high: true` to `false` change and
low-only test: an absent reference body proves neither behavior. A8/A9/AA's
write-test effects cannot be inferred from the known Info/Load operations.
The card source comments now distinguish the retained, unverified native policy
from the verified OpenBIOS event split; no behavior or coverage classification
changed. AF and the callbacks remain `Partial`, with no extra test/build needed
for these comment-only corrections. No active children or pending worker edits.

### CD live-event waits (`1fbe404`, ABI 18)

Parent replaced completion-Cell/Cycle selection in `cdrom.rs` with the live
TestEvent contract from pinned OpenBIOS `helpers.c` and `cdrom.c`
(`4dc70d2931121a0fde858a0354192b6df4fa22e8`, both read in full). Seek/status
poll DNE then ERR; read polls DNE, ERR, END. Only the winning flag is consumed.
Disabled, closed or already-consumed flags no longer release a wait just because
the native operation finished. Handles remain live if the guest reuses a slot.
Software events can wake before the CD deadline without advancing virtual time.

This uses a CD-specific condition in the existing `Wait` enum/driver, retaining
the per-operation `Rc<Cell>` for captured status and native reset cancellation.
`Wait` is now borrowed by `holds` rather than copied; other wait predicates and
scheduler stepping are unchanged. No event generations, register driver, clock
costs, runtime import/export or ABI version change. Cancellation remains an
explicit native error policy, not an INT5 producer, and cannot consume a newer
operation's notification. Normal TestEvent polling intentionally can: if a
callback clears DNE by starting another operation, the old waiter must wait for
a live flag and consume it, while retaining its own completed status byte.
The old reentry test's contrary event-consumption expectation was corrected.

An unready flag may wait indefinitely, as the reference polling loop does.
Absence of pending native CD work does not exclude a later guest DeliverEvent;
no new timeout/error policy is imposed. A premature software DNE before status
writes its local byte has native zero, not a claim to reproduce an uninitialized
retail byte. Command-acceptance loops and patched internal BIOS calls remain
unfinished. In particular, an early software read ERR followed by a returning
SystemError patch attempts SeekL while the original read is still busy: current
native acceptance rejects it with 0Bh and leaves that read active. The test
records this partial behavior; it does not silently cancel the read to make the
retry succeed. A5/A6 and the CD family remain `Partial`.

One grouped BIOS integration test covers disabled/consumed/reused events, early
software wakes and priority, winner-only consumption, reset cancellation and the
returning early-ERR/busy continuation. Existing reentry/retry tests are retained
and strengthened. One runtime driver test exercises both callback-delivered ERR
and callback reset, no deadline/clock advance, correct context and error ordering.
Its guest is a test double, not a newly compiled wasm guest patch.

Read-only DeepSeek review `6ba8ab1e-2063-45a3-84c2-7b5878d3a3a5` was read in full.
Parent accepted its core correctness review and added the two continuation/cancel
assertions above. Its proposed two-second no-pending-operation timeout was
rejected: that would change valid event-wait behavior, not merely diagnose it.
Its claim that cancellation coverage leaves interrupts disabled was also wrong
for A95, which exits critical. No other review-driven architecture was added.
The review completed despite an ENOSPC error writing its optional context mirror;
it made no source edits, ran no tests and needed no alternate launch channel.

Validation (all logs read, replay JSON stop/boot/report fields inspected):

- `cargo test -p psx-bios -p psx-runtime -p sdk-psyq --offline`: **53 / 9 / 42 pass**;
  `cargo check -p psx-runtime --target wasm32-unknown-unknown --offline`: pass.
  After review, the two strengthened tests passed again; production behavior was
  unchanged. Logs `cd-live-wait-{focused,integrated}-tests.log` in session scratch.
- Release CLI built successfully. The first BTN build exited 101 with an empty
  log while `/tmp` was full. One retry with `TMPDIR` in this worktree's ignored
  `out/tmp-cd-live-wait` succeeded for both games; no other sessions' files deleted.
  Native runtime and both game modules include the retry and live-wait changes.
  BTN **2,582,381 bytes / 86 adapters**; BOF4 **10,196,316 / 91**. All six chip
  groups native. Asyncify counts unchanged (1296/7158, 3009/18393, one exception
  handler excluded). Coverage stays BTN 42 working/12 partial/1 unsupported
  (B51)/1 fatal/1 noop; BOF4 36 working/16 partial/1 unsupported (A43)/1 noop.
- Four unchanged replays, explicit frames and `--check no-trap`, pass: BTN title
  6000 (all three expectations), card-load 3000 (no assertions), BOF4 title 26000
  (all five expectations), supplied crash 694 (no assertions). All boot text
  matches; EDC errors and guest I/O empty; final critical/in_irq false; no held
  waits or queued calls. Sector totals 1419/697/12695/680. BTN's frame-64 GPU
  timeout and resets at 65/83 remain unchanged and unclassified.
- Fresh disposable run storage; prior own cards preserved under
  `out/tmp-cd-live-wait/{btn,bof4}-previous-disposable-storage`. Build/replay logs,
  four replay JSON and `review-focused-tests.log` are in that same ignored
  directory because the session scratch filesystem was full. No user saves or
  inputs changed. No game save/reload certification, fresh browser/conformance,
  full-workspace/full-CI or non-empty oracle claim; prior gate epochs stand.

### Executable contract scout disposition

Read-only DeepSeek scout `7149ce1e-78db-4ae2-9e35-853c4f43832c` completed without
edits/tests. Parent read its full report and the pinned OpenBIOS
`kernel/psxexec.s` and `kernel/psxexe.c` bodies. Exec saves caller S0/RA/SP/S8/GP
into header offsets 38h/34h/28h/2Ch/30h, clears BSS, optionally changes SP/FP,
loads GP, calls PC with the two arguments, reloads those saved values from the
live header and returns 1. Existing Flow calls and SP/GP globals provide useful
seams, but saved RA/FP/S0 visibility and executable identity/admission remain
unresolved. No Exec/LoadExec code, classification or runtime ABI changed.

The report's proposed `Regs::code_known`/new guest import is **not an accepted
design**: table membership alone would not establish identity of newly loaded
bytes, and the scout did not establish that a new preflight query is necessary.
Its claim that LoadExec requires a clean thread/heap/event restart is not in
the quoted body, which loads and calls the boot executable without that reset.
The LoadTest/loadExe reference discrepancy and broken boot-path fallback are
not settled by declaring one interpretation the only functional one. A future
implementation still needs an evidence-based contract and real runtime/return
validation, not only a manually resumed Flow test. These open items are not
implicitly deferred with longjmp, nor is there a new approval request now.

### CD read-command dispatch follow-up (`a9b31c3`, historical checkpoint)

`CdReadSector` now invokes A(7Eh) through the live BIOS table instead of calling
`cd_read` directly. Pinned OpenBIOS `cdrom/helpers.c` (same `4dc70d...` reference,
read in full) calls `syscall_cdromRead(count, buffer, 80h)` up to **99999** times
until its v0 is nonzero. The native helper preserves that software call bound,
original arguments and live table re-lookup. It awaits a returned Call/Block
before continuing; immediate rejections loop without recursive Rust frames.
Exhaustion still calls patchable SystemError(44h,0Ch), then returns -1 only if
that handler returns. An accepted patch does not also execute the native read.
The existing live-event wait and ten-attempt read-error retry follow acceptance.
Cancellation attaches only to native work still active when acceptance returns.
A guest patch may reject after starting/cancelling native work, then satisfy a
later attempt using only software events; that successful attempt must not
inherit the previous operation's cancelled result. A reset during an already
active native wait still wakes that waiter as before. No fake event is delivered.

This is not instruction-cost timing: no time is added per attempt and an active
native operation is not silently cancelled or completed. Native busy progress,
SeekL/status command acceptance and dispatch, patchable TestEvent polling, CD
chain/INT5 services remain partial. No guest stack buffer, new ABI/import,
register driver, scheduler change or generalized retry framework was added.
A5/A6 classifications remain `Partial`.

One grouped BIOS-dispatch test covers second/last-attempt acceptance, exactly
99999 rejected patched calls, table changes between retries, v0-only boolean
results, a patch's saved native entry, no duplicate native start, two-sector
completion and the native immediate-rejection loop. Parent also covered the
cancelled-rejection/software-completion case and a native blocking table redirect
(A7E to A6), proving the replacement finishes before the read-event wait begins.
Guest patch execution is a manually resumed Flow test double, not a new
compiled-wasm test. Focused test and
`cargo test -p psx-bios -p psx-runtime -p sdk-psyq --offline` pass **54/9/42**;
wasm32 runtime check passes. Full logs are in session scratch as
`cd-read-dispatch-{focused,integrated}-tests.log`. Latest release/runtime/game
builds and four replays remain at `1fbe404`; browser/conformance/full-CI/oracle
and save-persistence claims are unchanged. No input or user-save writes.

Read-only DeepSeek review `4396461a-7529-4239-ae6c-baf9ed341834` accepted the core
change with notes; parent read the complete report. Its source review was based
on repo code/recorded contracts, not a fresh reference fetch (that role had no
network/shell tools). Parent had independently read `helpers.c`. The suggested
Block coverage is now in the same grouped test; historical checkpoints retain
their old scope while current summaries are updated. The Rc identity assertion
is retained alongside completion checks, not treated as sole proof of correctness.
The fallible-dispatch note does not justify a new Flow error channel: native
fatal/unsupported table redirects already use the established continuation panic
policy (as in the loader). Such redirects can fail, contrary to the report's
claim that the error path is unreachable. The parent-discovered stale-cancellation
fix above followed this review and passed the full focused integration gate.

### Krom2Offset contract scout disposition

Read-only DeepSeek scout `360adf88-4886-4e63-99e8-948b0436963d` and its bounded
clarification `bedc005b-2fa5-4d04-a991-a871456c5d49` completed without edits or
tests; both reports were read in full. Parent independently read psx-spx and the
full pinned OpenBIOS `charset/sjis.c` at `4dc70d2931121a0fde858a0354192b6df4fa22e8`.
B53 is a retail callable offset helper and does not itself require glyph bytes
or a ROM pointer. OpenBIOS's B51 multiplies its result by 30, supporting glyph
indices rather than byte offsets; psx-spx only says offset within charset.

The initial recommendation to copy `originalBiosTable` with a predecessor lookup
was rejected. Parent found that it maps **both 899Eh and 899Fh to 00BCh**, even
though both are valid Shift-JIS codes. The first-half rows lack the 7Fh skip
markers present in OpenBIOS's new-font table. Checking only charset endpoints
hides this discrepancy. The scout confirmed it and withdrew the recommendation;
the disabled supposed-retail switch is internally inconsistent too. Neither a
blind copy nor an inferred standard-JIS repair is accepted as retail behavior.
The scout's single additional DuckStation check supplied no mapping evidence.

No B51/B53 implementation, classification, glyph resource or identity policy was
changed. B53's missing mapping evidence is separate from B51's resource decision:
next work needs a reliable retail algorithm or authorized vectors, not a guessed
pointer, an unverified copied mapping, or a claim that fonts are complete.

### CD seek/status command dispatch follow-up (`5ac49b3`, historical checkpoint)

A5's seek phase now calls the live A(78h) table, and A6 calls A(7Ch), rather
than bypassing guest patches. Parent re-read pinned OpenBIOS `cdrom/helpers.c`
and `cdrom/cdrom.c` at `4dc70d2931121a0fde858a0354192b6df4fa22e8` in full:
SeekL has at most **99999** acceptance calls, status has **9**. Exhaustion calls
SystemError(44h,0Bh) / (44h,1Fh); event errors remain (44h,0Ch) / (44h,20h).
A returning error patch is still required before -1 can return. The existing
read acceptance helper now serves these three command sites; only v0 decides
acceptance, every retry re-reads the table, Call/Block finish before continuing,
and immediate rejection does not recurse in Rust or spend invented clock time.
Pending native work is neither cancelled nor advanced to force acceptance.

Both calls use the existing `Regs::sp/set_sp` and SDK `StCdInterrupt` frame
pattern: 24 bytes below the caller, including 16 bytes of o32 argument homes,
with MSF/status at frame+16. The frame survives calls and waits and is restored
after every normal return, including returning SystemError. Nested calls have
separate locals. MSF is written once per outer read attempt, not each acceptance
retry, so patches' byte edits persist through rejected seek calls. The retail
lost-minute retry behavior is unchanged. Status reads its actual guest byte
after DNE, not `last_status` or a synthesized return; an early software DNE can
expose the pre-existing guest stack byte before the native write. The minimal
frame is native implementation layout, not a claim to Sony's exact stack offsets.
No allocator, fixed scratch address, ABI/import, scheduler or exception framework
was introduced. Non-local-return work remains user-deferred.

Cancellation capture now uses the same small helper at all three accepted
command sites: only still-active native work can cancel the wait. Software-only
acceptance does not inherit a rejected attempt's old cancellation. This does
not cancel pending native work on early software DNE/ERR; a status command's
pending write still targets the pointer supplied to A7C, as for direct calls.
Patched B0B TestEvent polling, native busy progress, INT5 and chain services
remain open; A5/A6 remain `Partial`.

One grouped BIOS-dispatch integration test covers second/final acceptance,
exact exhaustion bounds, live table changes, v0-only boolean results, saved
native entries, MSF mutation, separate nested status locals, o32 argument-home
writes, SP restoration through waits/errors, stale cancellation and early DNE.
The existing runtime-driver CD test now has a mutable SP test double and a third
case: patched A7C supplies a local, a queued callback edits it and delivers DNE,
and the real driver returns the edited byte without advancing time, restoring
SP only after completion. Existing cancellation/error cases retain their checks.
These guest patches are Flow/Invoke test doubles, not new compiled-wasm fixtures.
Focused test and BIOS **55** / runtime **9** / SDK **42** plus wasm32 runtime
check pass; complete logs are `cd-command-{focused,integrated}-tests.log` in
session scratch. Fresh release/runtime/game builds and four unchanged replays
also pass for this source, as detailed below.

Read-only DeepSeek stack-seam scout `661d2e72-5f0b-4418-8332-ab386a1bf83b`
completed without edits/tests; parent read all 98 report lines. Its useful
argument-home/frame-lifetime observations are covered above. Parent independently
verified the status bound from the pinned source. MSF is read by native SeekL
at acceptance, not written at completion as one report sentence stated; it is
recomputed per read attempt, not per rejected command. Early status is guest
RAM, not nondeterministic Rust memory. No non-local/thread redesign or recursive
A7C-to-A6 redirect test is inferred from the report.

Independent DeepSeek review `4f40f9ef-17e8-49ae-b373-d6b36f8f6767` found no
established production defect and accepted with notes; parent read the complete
35-line report. That read-only role had no network/shell, so numeric reference
provenance is the parent's full-source read above, not an independent re-fetch.
The now-unused private `cd_get_status_at(None)` case is harmless and retained
rather than adding unrelated cleanup. Parent accepted two small test gaps:
returning END/SystemError restores SP, and resetting with no active operation
does not wake a software-only wait. Both assertions extend existing tests;
all six blocking-CD tests pass in `cd-command-review-focused-tests.log`.
The full 55/9/42 + wasm32 gate already includes the enhanced driver test; only
these extra assertions followed the gate/builds, with no production change.

Fresh native gate for this follow-up (ABI **18**, including `a9b31c3` read
acceptance): release CLI, runtime and both game builds pass. All six chip groups
remain native. BTN is **2,597,315 bytes / 86 adapters**; BOF4 is **10,211,434
bytes / 91 adapters**. Asyncify remains 1296/7158 and 3009/18393 with one exception
handler excluded. BIOS coverage is unchanged: BTN 42 Working / 12 Partial /
1 Unsupported (B51) / 1 Fatal / 1 Noop; BOF4 36 Working / 16 Partial /
1 Unsupported (A43) / 1 Noop. Named unsupported-call reporting remains intact.

The four unchanged replays pass explicit frame limits and `--check no-trap`:
BTN title **6000** (all three expectations), card-load **3000** (no state
assertions); BOF4 title **26000** (all five expectations), supplied crash **694**
(no state assertions). All stop at FrameLimit, boot text matches disc, EDC errors
are empty, `io_accesses` is empty, `crit/in_irq` are false and held waits/queued
calls are zero. Sector counts remain 1419/697/12695/680. BTN's GPU timeout at
64 and resets at 65/83 remain unchanged and unclassified, not silently removed.
Parent read the complete logs and parsed all four reports/check results.

Builds used the checkout's ignored `out/tmp-cd-live-wait` as TMPDIR. Logs and
copied replay/check JSON are `cd-command-{release,build-*,replay-*}` in session
scratch. Prior own run cards were moved there as
`cd-command-{btn,bof4}-previous-disposable-storage` before fresh disposable
storage was created. No input snapshots or user saves were written. No new
workspace/full-CI/browser/conformance/oracle run or save/reload certification is
claimed. Browser, memcard, GPU and pad/IRQ conformance retain their earlier
checkpoint-specific results, not refreshed claims for these native changes.

### CD TestEvent dispatch follow-up (`1fcc776`, historical checkpoint)

Blocking seek/read/status now call the live B(0Bh) TestEvent entry instead of
consuming ready flags themselves. Parent re-read the complete pinned OpenBIOS
`cdrom/helpers.c` and `cdrom/cdrom.c` at
`4dc70d2931121a0fde858a0354192b6df4fa22e8`: each pass probes DNE, ERR, then END
(read only), stopping at the first nonzero v0. Every probe reloads its handle
from the existing native CD state and its target from the guest BIOS table;
Call/Block completes before the next probe. A synthesized patch result selects
the tested event, not the arbitrary returned integer. Only the called TestEvent
or patch consumes flags; no automatic consumption/enable follows patch success.
Status still comes from the per-call guest byte, not the private completion
cell's now-unused high result word. Returning errors/retries and cancellation
retain their existing policies; no pending native work is silently cancelled.

An all-false round of the unchanged native TestEvent keeps the event-driven
`Wait::Cd` optimization. Its predicate now also wakes when the live TestEvent
entry is changed: a queued callback can install a synthetic completion patch
without delivering any CD flag. A round involving a patch cannot use that
predicate, since the next guest call can produce a different result without any
native flag changing. Such rounds run iteratively and yield after the existing
`LOOP_BUDGET` through `Wait::Cycle(now + quantum)`, resetting the existing guest
budget. This lets a no-loop wrapper around saved TestEvent reach scheduled
native completion instead of spinning forever in host dispatch. It is the
existing native loop/safepoint policy, not MIPS instruction-cost timing, a new
clock constant, timeout, event generation or scheduler framework. No runtime
production code, ABI, import/export or register driver changed. Run-report B0B
counts now include these dispatched internal probes; static support coverage
is unchanged, not a claim of count parity with older runs. Busy command
acceptance still does not advance native work; INT5/chain services remain open,
and A5/A6 remain `Partial`.

One grouped BIOS test covers seek/status/read probe order, v0-only truthiness,
live table/handle changes, preserved READY/DISABLED flags under synthetic
results, saved native TestEvent, error routing/retry, stack restoration, a
B0B-to-WaitEvent redirect, immediate-zero polling without Rust recursion, and
cancellation after a false-returning probe with all CD handles closed.
One runtime-driver integration test installs the patch from a queued callback
while the native wait is blocked. Synthetic success on the second polling round
returns at time zero; a saved-native wrapper with no loop reaches native status
completion after scheduler yields. The expected number of polling rounds is
derived from the actual command latency/quantum, and the test register provider
checks budget resets at yields (none for immediate success). Both paths keep
the guest local frame intact. Guest patches are Flow/Invoke test doubles, not
newly compiled-wasm fixtures.

Read-only DeepSeek scout `65d752ce-5015-4445-b98b-60cc2e59e497` completed;
parent read the full 74-line report. Poll ordering and wake-predicate warnings
were useful. Its proposed implicit consumption after synthetic success was
**rejected**: the reference caller does no extra flag write after TestEvent.
The existing native handle state remains authoritative and live between calls;
no speculative retail-address CD-global block was added. The report's description
of OpenBIOS retries losing minutes was imprecise: that source deliberately moves
MSF calculation outside the loop; our existing retail-quirk policy is unchanged.

Independent DeepSeek review `f4732701-01a3-4b70-baf2-3f89d840a244` completed
read-only; parent read all 30 report lines and accepted with corrections.
Parent fixed the bounded mid-round cancellation gap: after a false-returning
patch, cancellation is checked before another probe or quantum yield, not only
at the next round's first probe. The existing test now covers cancellation after
either status probe, including closed handles. The derived polling-count and
budget-reset assertions above address the review's test gaps. Optional removal
of round-local `patched` tracking was declined; its current behavior is correct
and does not force a quantum yield immediately after every patched round.
No re-review/full-wasm claim is inferred from the report. All changes received
parent source review and the final focused gate below.

Gates: initial and final BIOS **56** / runtime **10** / SDK **42** tests and
wasm32 runtime check pass, with complete logs read in session
scratch as `cd-testevent-{focused,integrated}-tests.log`. This bounded follow-up
does not rerun workspace/full CI, games, browser, conformance or empty oracles.
Release/runtime/game artifacts and four replay results remain at `5ac49b3`, not
fresh claims for this source. Browser/memcard/other conformance retain their
recorded epochs. No input, save, golden or replay expectation was changed.

### CD command-acceptance progress follow-up (`c830583`, historical checkpoint)

Long rejection loops in `cd_accept` now yield after each existing
`LOOP_BUDGET` of failed attempts, provided another attempt remains. The existing
`Wait::Cycle(now + quantum)` and guest budget reset let scheduled native work
complete before another command attempt. Immediate failures still iterate
without Rust recursion; Call/Block finishes before retrying, and every attempt
reloads the live command table with the original arguments. No implicit native
start/cancellation, event consumption or operation-result capture was added.

The software limits remain 99999 seek/read attempts and nine status attempts;
parent re-read complete pinned OpenBIOS `helpers.c`/`cdrom.c` at
`4dc70d2931121a0fde858a0354192b6df4fa22e8`. Success and final exhaustion never
add a yield. In particular, the short status loop can still exhaust without
advancing time. Long invalid-count failures now exhaust after 99 whole quanta.
With interrupts disabled, the existing scheduler still defers completion; the
retry loop does not force it or cancel the pending job. This is the existing
native loop policy, not instruction-cost timing or retail timing parity. Like
CD polling, it uses a local counter and resets the guest budget at a yield;
it does not decrement `Regs::budget` with `set_budget` per attempt or claim
shared guest-loop accounting. No runtime production code, ABI, import/export,
scheduler or register driver changed.
A5/A6 remain `Partial`: INT5/chain services and other CD internals are unfinished.

Existing BIOS integration tests retain exact bounds, arguments, live-table
changes, signed/v0 results, error codes, guest locals and stack restoration.
They now service Cycle yields and check exact elapsed whole quanta, including
success at attempts 1000/1001 and no yield after success/exhaustion. Masked busy
work remains pending at exhaustion. An early software read ERR now reaches a
busy-acceptance yield after the returning error patch, rather than necessarily
exhausting at time zero. One old TestEvent test initially expected that obsolete
zero-time exhaustion; it was corrected to explicitly mask interrupts for its
exhaustion case, and all seven blocking tests then passed.

One new runtime-driver integration covers A78/A7E with a pending native status
command: unpatched retries, callback-installed saved-native wrappers, and a
callback reset plus wrapper. A nondefault quantum derives the expected attempt
and budget-reset counts. The initial operation completes or is explicitly
cancelled as requested, the next command uses the live table and original stack
local, and exactly two native sectors arrive in the original destination. Guest
callbacks run in IRQ context, command patches outside it; the caller's SP is
restored. Patches are Invoke test doubles, not a new compiled-wasm fixture.

Read-only DeepSeek scout `9b5d2c3a-2bf6-46d1-a77d-2ec2966e9957` completed;
parent read all 153 report lines. Its bounded-retry, continuation, live-table,
short-status and masked-progress cautions agree with the implementation. No
additional architecture or timing-parity claim was accepted. Independent
DeepSeek review `b4db76ef-c12c-46da-a63a-18bdac082e83` completed read-only;
parent read its full 42-line report and accepted its OK-with-notes verdict.
The local-counter/budget distinction above and in the code comment addresses
its accounting note; no production change was required. Whole-quantum costs
are already asserted/documented. Its hypothetical 1500-attempt example does
not justify fractional waits: the one quantum pays for the preceding 1000
failures, not the remaining 500. Extra unmasked-long-busy coverage was optional
and not added; masked busy, invalid-count exhaustion and unmasked progress are
covered. No extra clippy/full-suite gate is inferred from reviewer boilerplate.
The reviewer ran no tests or network checks; reference validation and the actual
gates below are the parent's evidence.

Gates and artifacts for this batch (ABI 18):

- BIOS **56** / runtime **11** / SDK **42** tests and wasm32 runtime check pass.
- Release CLI and runtime/BTN/BOF4 builds pass; all six chip groups stay native.
  BTN is **2601079 bytes / 86 adapters**, BOF4 **10215230 / 91**. Asyncify remains
  1296/7158 and 3009/18393, with one exception handler excluded. Coverage remains
  BTN 42 Working/12 Partial/1 Unsupported/1 Fatal/1 Noop and BOF4
  36 Working/16 Partial/1 Unsupported/1 Noop; B51 and A43 remain named gaps.
- Four unchanged replays pass with explicit frame limits and `--check no-trap`:
  BTN title 6000 (all three expectations), card-load 3000 (no state assertions),
  BOF4 title 26000 (all five expectations), supplied crash 694 (no assertions).
  All stop at FrameLimit, match boot text, report no EDC errors or I/O accesses,
  and end outside critical/IRQ context with no held waits or queued calls.
  Sector counts remain 1419/697/12695/680. BTN's frame-64 GPU timeout and
  frame-65/83 resets are unchanged and remain unclassified, not newly certified.
- Complete logs and all four run/check JSON reports were read/parsed in session
  scratch under `cd-accept-progress-*`. Own previous disposable card directories
  were preserved there before creating fresh run storage. Builds/tests use the
  checkout's ignored `out/tmp-cd-live-wait` as TMPDIR; no input or user-save writes.
- No full workspace/CI, browser, conformance or empty-oracle rerun. Browser,
  memcard and other conformance results retain their prior recorded epochs;
  game save/reload persistence remains unverified. No replay expectations changed.

### CD file-read alignment and transfer follow-up (`8c28f9a`, historical checkpoint)

A(60h) `dev_cd_read` now rejects unaligned lengths/file positions, negative
lengths and positions at or beyond EOF with FCB error 16h and return -1. Both
public read aliases use this same device path; `lseek` still permits unaligned
positions, which the subsequent read rejects. Destination alignment is unchanged.
The native reader transfers every requested 800h-byte sector, then clips only
the successful byte count and FCB offset to the file's remaining length. A later
physical-read failure retains already-written sectors, records those actual
loaded ranges, leaves the offset unchanged and reports the existing error 10h.
Success leaves FCB/global errno unchanged. The request-sized temporary Vec is
removed; no new host API, CD driver, Flow, scheduling, ABI or shared-core changes.

Parent read full psx-spx kernelbios and pinned OpenBIOS `filesystem.c`,
`helpers.c` and `handlers.c` at `4dc70d2931121a0fde858a0354192b6df4fa22e8`.
The 800h alignment and `fpos>=filesize` errors are corroborated by psx-spx;
full-request copying, post-read clipping and FCB errno preservation follow the
OpenBIOS body. Partial-buffer retention follows its direct-buffer helper, not
an independent Sony oracle comparison. Read-only DeepSeek scout
`39a73ee5-d26a-4fc4-a4b1-e28e8e2afe5e` independently confirmed these mismatches;
parent read its complete 165-line report. Its sample's unchecked address adds
were not copied; native guest addresses use wrapping arithmetic. Its general
claim that successful functions preserve errno is too broad: this specific
read body does, whereas open clears it. No wider errno change was made.

A60/A02/B34 remain `Partial`. Status/path-table/disc-identity checks, command
error/callback sequencing and zero-count semantics remain unfinished. For a
zero-size request before EOF, the existing native no-op remains; the reference
instead reaches the count-zero command failure, not a demonstrated immediate
return contract. `CdGetLbn` is unchanged: the inspected OpenBIOS A4 slot is
`unimplementedThunk`; psx-spx describes first-entry/not-found bugs but does not
establish the missing lookup details. No mapping, default-version or garbage
result policy was inferred from that absence.

The existing device-table integration now requests complete sectors while
checking short-file results and unaligned-seek rejection. One new grouped
integration exercises B34/A02/direct A60: invalid requests leave buffers and
loaded ranges untouched; whole-sector writes extend past the clipped result;
nonzero starting positions work; a missing later sector preserves earlier
bytes without advancing the FCB. FCB versus global errno propagation and
adjacent buffer sentinels are checked. No new harness or compiled-wasm fixture.

Independent read-only DeepSeek review `5dfd50f2-1393-4aa0-a1f6-1cb9a83b97eb`
returned OK-with-notes; parent read its full 34-line report. No production
change was required. Its proposed EOF-error radix ambiguity is rejected:
psx-spx explicitly says **16h**, not decimal 16, consistent with `E_ALIGN`.
Optional duplicate failure coverage inside the file extent was not added;
the same failure branch is already exercised past logical EOF. A test pinning
the acknowledged incomplete zero-size no-op was also declined. The reviewer
ran no tests or network checks; reference verification and gates are the parent's.
Its loader observation applies to header reads only: body sizes come from the
header and are subject to this device's validation (body results remain ignored
by the existing Load contract). No claim that all loader reads are 800h was made.

Gates and artifacts for this batch (ABI 18):

- All eight file tests pass, then BIOS **57** / runtime **11** / SDK **42** and
  the wasm32 runtime check pass; complete logs read, no failing tests.
- Release CLI/runtime/both native game builds pass. BTN **2600699 bytes / 86
  adapters**, BOF4 **10214848 / 91**; all six chip groups native. Asyncify and
  BIOS support counts are unchanged from `c830583`; B51/A43 remain named gaps.
- All four unchanged replays pass with explicit frame limits and no-trap:
  BTN title 6000 (three expectations), card-load 3000 (no state assertions),
  BOF4 title 26000 (five expectations), supplied crash 694 (no assertions).
  All stop at FrameLimit, match boot text, have no EDC errors/I/O accesses,
  and end outside critical/IRQ context with no held waits/queued calls.
  Sector counts remain 1419/697/12695/680; BTN's existing timeout/reset logs
  remain unclassified. These game gates do not independently prove the new
  file-read edge cases or certify save persistence.
- Full logs and four run/check JSON reports read/parsed in session scratch
  under `cd-file-read-*`; previous own disposable card directories preserved
  there before fresh run storage. Tests/builds use ignored `out/tmp-cd-live-wait`
  as TMPDIR. No input/user-save/golden/replay-expectation writes.
- No full-workspace/CI, browser, conformance or oracle rerun. Those gates keep
  their previously recorded epochs and unresolved prerequisites.

### CD file-read Flow follow-up

A60 now calls the existing `cd_block_read` helper directly and sequences its
Flow before returning the byte count or setting the FCB error. This removes the
separate HostDisc loop: native file reads use the shared drive's seek/read
completion, live A78/A7E/B0B command/event patches and A1 SystemError path.
The reference calls `cdromBlockReading` directly, **not through A(A5h)**;
patching that public slot does not intercept the internal call. No extra
scheduler, clock cost, BIOS register driver, runtime or ABI change was added.

The sector/EOF precheck remains immediate and leaves invalid-request buffers
untouched. The request's count, starting LBA and destination are captured before
the helper runs. Only a full-count result succeeds; after it, offset and length
are read again from the live FCB, with the reference's unsigned 32-bit
comparison/subtraction/addition, and only the result/offset are clipped. Patches
can therefore change the final file position without retargeting the pending
read. Success does not clear errno. Completed native sectors and loaded ranges
come only from `cd_step`; the file function does not repeat the transfer.

Missing-sector END now calls SystemError(44h,17h); offset/error handling waits
for that call to return. The default SystemError remains fatal, not an ordinary
file-error return. A returning patch lets A60 set FCB error 10h and return -1;
public read aliases then copy that error globally. Existing read-error retries
and cancellation behavior are reused. A zero-byte request before EOF now reaches
the helper: seek, then 99999 rejected native ReadN attempts and SystemError
(44h,0Ch), not the former successful no-op. At/beyond EOF it still fails the
initial alignment/EOF check. BIOS CD events must be installed; normal boot does
this already. There is no implicit initialization or direct-host fallback after
removal. Initial boot's separate executable reader is unchanged.

Parent independently read pinned OpenBIOS `filesystem.c`, `helpers.c`,
`helpers.h` and `common/psxlibc/stdio.h` at
`4dc70d2931121a0fde858a0354192b6df4fa22e8`. Read-only DeepSeek scout
`0d170c5e-461b-4c03-a4f7-de50617bba42` completed; its full 253-line report was
read. Parent corrected the initial brief's A05/A5 notation and unverified table
assumption before implementation; the scout confirmed the direct-call seam.
Its proposed saturating subtraction for callback-edited FCBs was rejected:
`offset`/`length` are uint32_t, and a safety clamp would invent a new contract.
Its claim that the FCB lacks an identity field is also incorrect: DEVICE_ID
exists, but native CD identity population/checking remains unimplemented.
A60/A02/B34 stay `Partial`: status/path-table/disc-identity checks, plus the
helper's remaining INT5/chain-service limitations, are not completed here.
This is reference-backed behavior, not independent Sony oracle proof.

Tests extend the existing file integration rather than adding a harness:

- Native device-table reads initialize the normal CD events and drive existing
  Flow waits. Sector data fixtures now include valid EDC. Invalid requests,
  whole-request copying, clipped tails, red zones and partial failure remain
  covered across B34/A02/direct A60, with deferred offset/error and guest-SP checks.
- One new grouped test patches Seek/Read/TestEvent, writes software data with no
  physical disc, changes live FCB metadata, checks exact operands/continuations,
  and poisons A(A5h) to prove the internal-call distinction. It also exercises
  zero-count rejection through a returning SystemError patch. These are Flow
  callback test doubles, not a new compiled-wasm fixture.
- First focused compile caught two test assertions accessing private `Cd::op`;
  they were replaced with the existing clock's pending-Cd query, without changing
  production visibility. Then all **9** file tests passed. BIOS **58** / runtime
  **11** / SDK **42** and wasm32 runtime check pass; complete logs read.

Independent read-only DeepSeek review `55a9fe7d-a579-437b-88fa-b5654064fe88`
returned OK-with-notes; parent read its full 31-line report. No established
production defect required a change. Default fatal errors, required CD events
and the inherited retail read-retry minute-loss quirk are intentional consequences
of reusing the helper, not reasons for a fallback or new error policy. Its claimed
32768-sector/cancellation collision is rejected: the wait's ERR result is converted
to SystemError/-1 inside `cd_block_read`, never returned raw to A60. Missing CD
events exhaust **one** 99999-attempt Seek loop, not two loops as the review says.
The new high-word-nonzero acceptance case alone does not distinguish v0-only
handling; the existing helper tests already cover high-word-only false values.
Parent accepted a bound on the file test helper's scheduled-wait stepping to
avoid hanging on a regression; all nine file tests pass again after this test-only
change. No repeated build or full gate was needed; production source is unchanged.
The reviewer ran no tests or reference fetches; reference verification and gates
are the parent's. Optional duplicated helper cancellation/default-fatal tests were
not added. Historical `8c28f9a` text is labelled as such; current gaps are above.

Fresh integration gates (ABI 18): release CLI, runtime and both normal native
game builds pass. BTN **2603727 bytes / 86 adapters**, BOF4 **10217908 / 91**;
all six chip groups native. Asyncify and BIOS coverage counts remain unchanged.
All four unchanged replays pass explicit frame limits/no-trap: BTN title 6000
(three expectations), card-load 3000 (none), BOF4 title 26000 (five), supplied
crash 694 (none). All match boot text, have no EDC errors/I/O accesses and end
outside critical/IRQ context with no held waits/queued calls. Sector counts
remain 1419/697/12695/680; BTN's existing timeout/reset logs remain unclassified.
The reports contain **no A60 calls**: these are regression gates, not proof of
the new file path or game-level save persistence.

Full logs and four run/check JSON reports are read/parsed in session scratch
under `cd-file-flow-*`; previous own disposable card directories are preserved
there before fresh run storage. Tests/builds use ignored `out/tmp-cd-live-wait`
as TMPDIR. No input/user-save/golden/replay-expectation writes. No full-workspace/
CI, browser, conformance or oracle rerun; those gates retain their recorded epochs.

## Progress log

- 2026-10-31: callable kernel functions and callback ordering integrated as
  described above; shared native/fallback state and baseline scheduling remain.
- 2026-10-31: file follow-up, fallback batch and independent renderer/browser
  fixes source-reviewed and integrated. Combined foundation builds and replay
  gates now run as recorded above. Missing BIOS families and control-flow
  limitations below remain; this is not a full-BIOS completion checkpoint.

- 2026-10-31: parent integrated saved BIOS entry thunks in `bfc37c8`, with
  **ABI 17** in runtime/link/worker/conform contracts. Baseline masked dispatch
  is retained; low-kernel slot collisions are rejected. An actual game-mode
  wasm test covers saved and constant pointers, a lifted direct call, table
  replacement, four arguments, 64-bit return and RFE unwinding. Link-pass tests
  6, linker 9, BIOS 23 pass; wasm32 runtime check passes. The ignored threaded
  Docker/DWARF test was not run.
- 2026-10-31: oracle backend/vectors integrated as `dc03017` after source
  review: 42 unit tests and CLI check pass. Docker tests skip without the
  configured images; no emulator comparisons or new captures were run. Sony/
  OpenBIOS disputed cases retain their existing separate verdict treatment.
- 2026-10-31: `bb68c6f` recovers ResetEntryInt's default native RFE buffer and
  syscall >=4 exception-event delivery. BIOS 24 tests pass. The existing
  syscall seam still cannot preserve incoming v0; that remains a limitation.
- 2026-10-31: SDK pad RAM state/own elements integrated as `f8f015a`, `ae58f54`;
  SDK 36 and runtime 7 tests plus wasm32 runtime check pass. Worker inspected
  real PsyQ 4.7 objects; parent verified libapi's helper write/dequeue order
  against the actual object too. This is static object evidence, **not** SDK
  conformance or a game gate. Digital-pad-only behaviour and baseline wait
  accounting remain; no BIOS serial driver was added. Full workspace, release
  CLI, game builds, conformance and replay gates are still pending.
- 2026-10-31: integrated wave two: division `d8ef9df` (parent additionally
  fixed unparenthesized nested operands), native GPU APIs `466fba9`, SDK IRQ
  RAM state and object-static binding `2bb22aa`. Integrated gates: C 20,
  link-pass integration 5, BIOS 23, SDK 36, runtime 7 tests; build check passes.
  Real SDK object/conformance and game gates still pending; worker fixtures
  are not evidence that those gates passed. Parent libc review reproduced
  `%n%s` reading stale data, then fixed immediate writes and the mod-log
  caller in `1dc3509`; BIOS 23 tests and wasm32 runtime check pass.
- 2026-10-31: recovered RAM-only interrupt chains (`7b24e6c`, adapted
  `6155844`), native card/pad gating (`a64e1b2`, `d7d8f48`) and kernel
  handler ordering (`e3e4374`, adapted `053035d`). Pad sampling still calls
  the host directly; only shared IRQ acknowledgement/masking uses registers.
  `strtok` moved to 7200h (`e0a936e`) to avoid the handler elements at 7100h.
  Integrated BIOS/SDK/runtime gates passed: 19/35/7 tests.
- 2026-10-31: recovered timers, UnDeliverEvent and kernel allocation in
  `4e4e74e` (adapted `d89a101`/`b9db230`). Timers call the existing core;
  kernel allocation handles an exhausted event-block region without unsigned
  underflow. Recovered guest-visible call tables, aliases, patched dispatch,
  vector compatibility data and BOOT arguments in `9cf627d` (adapted
  `4ec0ec4`). Vector bytes are data, not executed BIOS code. BIOS/runtime/lift
  tests pass: 21/7/17. Saved-entry pointer thunks and the accompanying ABI
  version change were pending at this checkpoint; now recovered in `bfc37c8`.
  SDK calls through patched entries remain pending. No game/workspace gate or
  full-BIOS completion is claimed.
- 2026-10-31: integrated the reviewed DeepSeek lanes: compiler lowering
  (`864002e`..`030497a`), lifter correctness (`052b01f`..`f381c11`) and native
  BIOS libc/heap/printf (`8fcc9d4`). No map/timing or TR2/TR3 rules recovered.
  Parent recovered narrow-argument/variadic marshalling, unnamed writable-data
  rejection, signed-char propagation and constant indirect-call lowering (the
  last adapted without the upstream timing-dependent bulk-memory machinery).
- 2026-10-31: recovered ResetEntryInt/event/card/descriptor corrections,
  SYSTEM.CNF stack selection, native PAD_init2/PAD_dr and RAM-backed event
  blocks with reporting (`435d8e8`). Card/pad transports remain native. Targeted
  gates passed at integration points: BIOS 18, C lowering 18, lifter 17, linker
  9, link-pass integration 4, modkit 8 tests; runtime checks pass. The existing
  Docker/debug threaded-DWARF integration test remains explicitly ignored.
  Workspace/game gates have not run. Source-placement division correction and
  SDK-state salvage remain pending, as do the later BIOS services.
- 2026-10-31: delegation tools restored. Confirmed the requested DeepSeek model
  identifier and prepared isolated lifter, C-lowering and BIOS-library lanes.
  Parent retains architectural and integration authority.
- 2026-10-31: created the archive and recovery branch/worktree; preserved the
  user's instructions and working files. Baseline `cargo test -p psx-bios`
  passes in the recovery worktree: 12 tests, 0 failures. No source changes or
  recovered correctness patches yet; full foundation and game gates remain
  pending. Delegation is blocked as recorded above.
- Planning: compared the two pinned revisions and inspected A0/B0/C0 dispatch,
  aliases, card/file implementations, thread/runtime paths and the psx-spx retail
  list. Found 29 unhandled slots plus the partial behaviours above. This was
  source inspection, not a new conformance run. No implementation has started.

## Findings

- The old handoff is incomplete: it omitted A(41h/42h), the five C0 zero-return
  entries and several named stubs. Its "keep" list also contains code that still
  needs native-backend reworking or completion.
- The existing thread implementation and specialised exception continuations
  must not be mistaken for absent functionality just because direct BIOS paths
  have limitations.
- A(52h) has conflicting descriptions in psx-spx. Resolve against the applicable
  retail implementation before choosing its behaviour.
- This inventory establishes missing handlers and explicit partial paths; it
  does not certify every branch of every remaining implementation. Record
  concrete additional defects encountered during this work without expanding
  into another project-wide fidelity audit.
