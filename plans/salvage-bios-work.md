# Salvage the BIOS work

**Status:** in progress; recovery foundation and initial parallel salvage.
**Goal:** preserve the working native-port architecture, recover useful work from
spec-fidelity, and finish the missing BIOS functionality in Rust.
**Supersedes:** [spec-fidelity.md](spec-fidelity.md) as the execution direction
for this recovery and BIOS completion. That plan remains historical evidence,
not a checklist to resume.

## Read first

- [AGENTS.md](../AGENTS.md): smallest change, few tests, mostly integration.
- [Emulation layers](../docs/emulation-layers.md): the architecture to preserve.
- [Conventions](conventions.md): inputs, data hygiene, commands and commits.
- [Handoff](handoff-spec-fidelity.md): history and candidate commits. Its
  keep/remove classifications are leads to inspect, not authoritative decisions.
- [Design](../docs/design.md): affected sections and D67–D71. Those decisions
  were recorded without the user's input; their presence is not approval to
  retain the architectural changes.
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

## Work order

### 1. Recover a native foundation

- [x] Preserve the audited head on an archive branch. Recheck for later user
  changes before branching; do not overwrite existing branches.
- [x] Create a separate salvage branch/worktree from the pinned baseline.
  Preserve this plan, the handoff and the crash replay for use there. Leave main
  and the original working tree intact.
- [ ] Recover independent correctness fixes in coherent batches. Split mixed
  commits or reimplement their small useful part; do not cherry-pick a chain
  that drags the rejected map/timing work back in.
- [x] Record dispositions for unrelated useful changes, including residency,
  browser storage and fallback-device fixes. Keep this separate from the BIOS
  completion checklist.
- [ ] Build and check the recovered foundation using the existing commands.
  Record actual results and pre-existing failures, not assumed green status.

**Exit:** a buildable recovery branch retaining the original native/fallback
architecture, without phase 6/7. Nothing has been destructively reset or merged
back to main.

### 2. Salvage the useful BIOS changes

- [x] Recover C-library and allocation additions and existing-API bug fixes.
- [ ] Recover event, timer, pad and card API improvements. Retain the existing
  thread implementation; do not replace it under the heading "implement threads".
- [ ] Recover guest-visible BIOS tables, events, handler chains and file/device
  blocks where needed by their interfaces. Rust implements the functions;
  populating data does not require executable Sony BIOS code.
- [x] Recover general file/device dispatch, including guest-provided device
  callbacks. Keep useful CD directory and error behaviour while replacing the
  controller-driven reads with native disc operations. Console input and card
  asynchronous visibility remain separate unfinished items below.
- [ ] Implement the added BIOS GPU/CD entry points over the existing native
  cores and scheduler. Reuse SDK/backend facilities without depending on SDK
  initialisation or duplicating their state.
- [ ] Restore native pad/card access while preserving corrected buffers,
  busy/error states, completion events and persistence. Native and fallback
  callers must see one underlying card/input state, not separate BIOS copies.
- [ ] Recover boot fixes and BIOS oracle support. Keep ABI/linker/browser glue
  consistent with the recovered architecture; rebuild artifacts together and
  reject incompatible ones rather than mixing ABI 12–16 outputs.

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

All 29 slots below currently reach `not implemented` after alias resolution.
Five are documented no-ops, and A(52h)'s meaning needs verification.

| Done | Entry | Function |
|---|---|---|
| [ ] | A(41h) | `LoadTest` |
| [ ] | A(42h) | `Load` |
| [ ] | A(52h) | Resolve reference conflict: summary says `GetSysSp`, another section says `SystemError`; then implement the verified behaviour |
| [ ] | A(9Ch) | `SetConf`: main-thread reset implemented; live non-main reset still blocked |
| [x] | A(9Dh) | `GetConf` |
| [ ] | A(A7h) | `bufs_cb_0` |
| [ ] | A(A8h) | `bufs_cb_1` |
| [ ] | A(A9h) | `bufs_cb_2` |
| [ ] | A(AAh) | `bufs_cb_3` |
| [ ] | A(AEh) | `bufs_cb_4` |
| [ ] | A(B2h) | `ioabort_raw` |
| [ ] | A(B4h) | `GetSystemInfo`: size/flags implemented; date/version identity unresolved |
| [ ] | B(4Dh) | `_card_info_subfunc` |
| [ ] | B(53h) | `Krom2Offset` |
| [ ] | B(58h) | `_card_chan` |
| [x] | C(04h) | `get_free_EvCB_slot` |
| [x] | C(05h) | `get_free_TCB_slot` |
| [x] | C(08h) | `SysInitMemory` |
| [ ] | C(09h) | `SysInitKernelVariables` (reference notes its post-boot corruption; verify the callable behaviour) |
| [x] | C(0Eh), C(0Fh), C(10h), C(11h), C(14h) | Five retail zero-return entries |
| [ ] | C(15h) | `_cdevinput` |
| [ ] | C(16h) | `_cdevscan` |
| [ ] | C(17h) | `_circgetc` |
| [ ] | C(18h) | `_circputc` |
| [ ] | C(19h) | `_ioabort` |

`LoadTest`, `Load` and the five zero-return entries were omitted from the old
handoff's missing-call list.

#### B. Named calls and paths that are incomplete

- [ ] **General `setjmp`/`longjmp`, A(13h/14h).** The Rust `setjmp` only returns
  zero and `longjmp` always errors. Recognised lifted `setjmp` sites already save
  registers, and exception-hook continuations already work. Extend what exists
  for general non-local returns; do not mistake the direct Rust stub for the
  entire current implementation.
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
- [x] **`SetMem`, A(9Fh).** Updates the guest-visible RAM-size variable for
  retail's 2/8 MB choices, also reported by GetSystemInfo(5). No host memory
  resizing or console-wide bus map.
- [ ] **Card auto-format, A(ADh).** The flag is written but never read. Implement
  the documented opt-in behaviour and its initialisation/reset, with tests using
  disposable card images. Never test destructive formatting on user saves.
- [ ] **Card waiting/completion.** `_card_wait` drains operations immediately;
  asynchronous file I/O moves all bytes before queuing completion notifications.
  Resolve buffer visibility, busy state and event/wait ordering using scheduled
  native operations. Do not restore serial exchanges or instruction timing.
- [ ] **Console input/control.** `gets` always errors; built-in blocking TTY
  reads error; low-level TTY `ioctl` is a zero-return stub. `getc`/`getchar` and
  output wrappers already exist. Complete the missing software and host-input
  integration, reusing file/device dispatch.
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
  logs rather than delivering the unresolved-exception event/error; SDK adapters
  reject patched BIOS-table targets; non-CD built-in kernel-handler addresses
  lack callable implementations. Fix meaningful native call/exception paths
  through the existing driver, guest calls and lowering.
- [ ] **Coverage reporting.** `bios::implemented()` currently mistakes some
  named stubs/failures for completed functions. Make build reporting distinguish
  working support, partial support, unsupported calls and documented fatal/no-op
  behaviour. Keep this simple; no new coverage framework.

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

- [ ] Run workspace tests and build both games from the recovery branch's own
  toolchain/artifacts. Refresh census inputs when recovered compiler changes
  require it.
- [ ] Run existing BIOS oracle checks and focused integration tests for changed
  families. Keep emulator/OpenBIOS results provisional where appropriate.
- [ ] Run existing replays, including btn save/load, bof4 `title-to-field` and
  the supplied name-select crash replay. Check native/fallback integration and
  the mod/browser paths affected by the recovered ABI.
- [ ] Run `psxw ci btn` and `psxw ci bof4`; classify unresolved failures rather
  than calling the branch green. Hand off long/manual runs when necessary and
  leave their tasks pending until results are available.
- [ ] Update affected design sections, D67–D71 dispositions, the plan index and
  open items to describe what actually remains. Do not preserve conflicting
  claims that spec-fidelity or all BIOS functions are complete.
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
- Full `psxw ci btn` / `psxw ci bof4` and browser disc isolation remain pending.

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
The separate ABI-17 foundation CI remains active and pinned to its own source.

Native-CD's initial `0aac185` is **not accepted or integrated**. Parent review
found that a blocking multi-sector read wakes after its first sector, normal
completion is confused with the early-end event, and changes to `Drive.status`
do not update the controller's actual shared status or pending jobs. The worker
is resumed as `b498c913-ceb9-47d3-8fe5-22dc437e8163` for bounded corrections,
including event reuse/allocation cleanup and existing core error semantics.
No register driver or native-core/SDK ownership rewrite is authorised.

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
