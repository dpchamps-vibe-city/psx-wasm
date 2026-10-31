# Open items

Open tasks, blocked items, human-needed input, unresolved findings and errata.
Milestone history is in [`completed/`](completed/). Older audit findings below
retain their provenance; the active recovery status is recorded separately.

- **Active plan:** [`salvage-bios-work.md`](salvage-bios-work.md), native recovery
  integrated with BIOS completion still in progress. Its checkpointed results
  supersede older coverage/gate claims below.
- **Last full gate runs:** both `psxw ci btn` and `psxw ci bof4` completed at the
  ABI-17 recovery foundation (`a74f832`, code `4cce9e3`) and exited 2. Missing
  oracle prerequisites, BTN mods census inputs and two BTN difftest mismatches
  remain unresolved; regression attribution is not established. Later ABI-18
  focused tests/builds/replays do not constitute a fresh full-CI pass.
- **The hardware boundary** (native adapters vs per-chip device fallback) is
  described in [`../docs/emulation-layers.md`](../docs/emulation-layers.md) and
  [`unified-adaptor-layer/`](unified-adaptor-layer/).
- **Shortcuts** (42, 2026-10-30 audit) are tracked in
  [`shortcuts.md`](shortcuts.md); the audit's bugs are under *Bugs* below.

## Native BIOS recovery: current gaps

- **General `setjmp`/`longjmp`: deferred by the user.** Recognised setjmp sites
  save registers and have compiled continuations, but an ordinary continuation
  call does not unwind intervening frames and returns to the wrong caller.
  Dependent C(19h)/A(B2h) `ioabort` remains unfinished. Note the bug and revisit
  later; no control-flow design or implementation is pending approval now.
- **Other control-flow gaps:** build-known Exec/LoadExec, warm boot, C06 hook
  chaining, remaining exception context and callback/IRQ thread switches are
  still open. A(40h) now has its documented fatal handling and patchable table
  dispatch, with focused tests passing; BREAK event/exception entry is still
  count-only. Do not replace existing Asyncify threading or infer approval for
  a runtime redesign from the recovery work.
- **Partial software services:** card helpers A7..AA/AE, built-in console input
  and circular helpers, CD internal calls/filesystem details, and
  non-main SetConf. Recent card Info/Load callback and guest-device console-scan
  fixes do not complete these whole families. AF write-test notifications remain
  unverified: OpenBIOS's missing body does not justify adding or removing them.
  BIOS CD opens now honor explicit
  ISO versions, default to `;1`, and support `?` filename matching without star
  expansion; shared disc lookup and CdGetLbn are unchanged. CD file reads now
  enforce 800h length/position alignment and reject positions at/beyond EOF.
  They now use the existing native blocking status/read helpers directly,
  honoring live command/event/error calls and scheduled completion, not the
  A(A6h/A5h) table slots. Status bit 10h rejects before any seek/read; returning
  status errors do too. The read's LBA/offset are obtained after status callbacks.
  Full requested sectors transfer before clipping against live post-callback
  FCB metadata. Completed sectors survive later failure; FCB error 10h/return -1
  occur only if SystemError returns (its default is fatal). Success preserves
  errno. Zero-count reads before EOF now reach the helper's bounded command
  failure, not a no-op; installed BIOS CD events are required, as boot provides.
  Path-table/disc-identity checks, status checks in other file operations and
  native shell-open/acknowledgment behavior remain partial. No new CD driver or
  scheduler was added. CD read errors now
  retry after a returning SystemError patch, bounded to ten attempts. Blocking
  calls now use live ordered event flags, including early software wakes and
  disabled/consumed/reused slots and native cancellation. Seek/read/status now
  call live A78/A7E/A7C entries with at most 99999/99999/9 acceptance attempts,
  preserving arguments and awaiting patched continuations. MSF/status locals
  use per-call guest stack frames; status reads the actual byte after DNE.
  Software completion after a rejected/cancelled attempt does not inherit that
  stale native cancellation. Completion probes now call patched B0B TestEvent
  in order, honoring Call/Block and synthetic results without implicit flag
  consumption. Native waits wake on table changes; patched polling yields on
  the existing loop budget/quantum so wrappers cannot starve scheduled work.
  Long command-acceptance loops now use the same local budget/quantum yields,
  letting busy native work complete before retrying the live command table.
  The 99999/99999/9 bounds remain exact, with no yield after success or final
  exhaustion; nine status attempts can still exhaust without a yield. This
  resets, but does not spend, the guest budget per attempt or claim retail
  timing parity. INT5/chain services remain unfinished. An early read ERR can
  still encounter busy SeekL after the patch returns, but long retries now
  allow scheduled progress; no silent native-read cancellation is added.
  A52/C09 contracts remain unresolved.
- **Resources/policy:** legally usable font data and GetSystemInfo's BIOS
  date/version identity remain undecided; no fabricated values or pointers.
  B53's offset calculation itself needs no glyph resource, but the proposed
  OpenBIOS original-font lookup aliases valid 899Eh/899Fh codes. Parent rejected
  that mapping; reliable algorithm/vector evidence remains a separate gap.
- **Validation:** the CD file-status follow-up passes BIOS 59 /
  runtime 11 / SDK 42 tests and wasm32 runtime check. The initial focused run
  found a test counter assertion counting the wrong redirected native callee;
  corrected before the passing gate. Parent reviewed the diff and contract scout.
  Logs are in session scratch under `cd-file-status-*`. No new game build/replay
  cycle: release/runtime/game artifacts and four passing replays remain at
  `15d15f3`, before the status change. Those replays do not call A60; file
  integration tests cover it. All six chip groups were native in those builds;
  ABI 18 and coverage classifications are unchanged. Previous logs, run/check
  JSON and disposable run cards remain under `cd-file-flow-*`. Tests use ignored
  `out/tmp-cd-live-wait` as TMPDIR. Inputs and user saves were untouched.
  Following the operator's concern about prolonged tail work, no new follow-up
  lanes are being opened at this checkpoint. Full BIOS remains unfinished;
  the substantive gaps above are not merely final cleanup.
  BTN's existing GPU timeout/reset logs remain unclassified.
  Latest memcard conformance remains at `99fa333`. Non-empty oracle
  validation, unresolved full-CI failures and save/persistence certification
  remain open. Two-bundle browser disc isolation, locked-import refusal,
  unlocked replacement and copy-error writer cleanup now pass 25 checks each
  in Chromium/Firefox at `8e5badf`, through production pages and real OPFS
  handles, retaining the seven card checks. At `9624526`, fresh ABI-18 BTN
  browser bundles also pass five conformance scenarios and the 6,000-frame
  title/recording replay in both browsers, with native RAM/VRAM/audio matching.
  GPU-oracle goldens and BTN mod inputs remain missing; these focused gates
  do not constitute full CI or game save/reload certification.

See the active plan for exact service inventories, accepted source changes and
next actions. Deferral or an unsupported diagnostic does not complete a service.

## Needs the human

- **`save` and `load` recordings.** Record two sessions in the page
  (`web/README.md`): `save` plays until the game saves; `load` boots and
  loads that save, with the card from the first. Store them as
  `games/btn/replays/save.rpl` and `load.rpl`. Scripting them from frame
  dumps was tried and dropped. Unblocks M7 task 4. *From:* M7 task 4.
- **`one-day` recording.** One full in-game day of btn, recorded the same
  way, stored as `games/btn/replays/one-day.rpl`. Unblocks M7 tasks 5–6.
  *From:* M7 task 5.
- **The pacing machine.** Add `machine = "…"` to `psxw.local.toml` so
  `web-pacing` gates instead of reporting. *From:* follow-up tasks.
- **The PsyQ `.LIB` files** (and headers), named in `psxw.local.toml`.
  Unblocks M8 task 5 and the member layout task 6 needs. *From:* M8 tasks
  5–6.
- **A capture rig:** an original PS1 with a serial port, a USB-serial
  cable at the right levels, and Unirom or FreePSXBoot. Or a community
  member with a rig who runs exported batches (`psxw oracle export`) and
  returns goldens. Unblocks M10 tasks 1–4 and 7. *From:* M10.
- **Approval to push psx-decompilation-harness.** The human said not to
  push (2026-10-29). Its M13 work is merged into local `main` (`00c41b7`).
  bof4-decomp `57f7da1` names `main` and has it checked out locally, but a
  fresh clone can't install the harness until it's pushed. Blocks M13 task
  5 and exit criteria 1–2. *From:* M13 task 5.
- **The second game (Q4).** The human picks from a candidate evaluation
  (M9, below). *From:* M9 task 1.
- **Entity interactions in the page.** On btn's farm: talk to NPCs, pick
  up the dog, collide with stones, weeds and sticks. The cause was fixed
  (bug-follows §2: the linker's o32 rule, `e3f65f1`; the decomp, btn
  `b37ae28`), but no replay covers it: `title-to-gameplay` walks into
  nothing. Confirm by play, or author a replay with an `expect`. *From:*
  follow-up tasks; bug-follows §2.

## Blocked on CD timing (D48)

- **`ramdiff` to zero differences.** M4 task 11, M6 task 10 and M7 task 6
  (`psxw oracle ramdiff btn --replay one-day`, M7 exit criterion 4) ask
  for it.
  - **Why it can't get there:** btn steps its RNG on every pass of its
    loading loop, so state after the first load depends on load time. The
    two drive models disagree on load time (M7 Findings: at counter 791
    psx-wasm stalls 110 frames, Beetle 182), and Beetle adds seeded jitter
    to each CD command.
  - **Ruled out:** patching Beetle to record its CD events (the human's
    call).
  - **Routes if revisited:**
    - run btn's libcd as original code over the register-level CD
      controller, then find an unpatched way to share the emulator's
      timing. The controller exists (D58) and bof4 runs over it; btn keeps
      its validated libcd adapters under `adapter-or-device`;
    - or settle for equality up to the first load.
  - *From:* follow-up tasks; M4, M6, M7.
- **Other difference classes `ramdiff` shows.** Each needs a design or a
  justified exclusion:
  - libcard's BIOS-dependent self-patching (`_patch_card*`);
  - libetc's interrupt stack inside `Hcount`'s map range;
  - libcd's internal state, which stays in the runtime under adapters;
  - `Vcount`.

  *From:* M4 Findings.
- **Emulator title-menu timing.** The menu appears about 10 counter steps
  later on Beetle than natively, so a press recorded just after it appears
  natively (`start-no-cutscene-interaction`, UP at 286) is lost there.
  Loads tick the counter differently too. Replays recorded in the page
  don't steer the emulator through the title menu. *From:* follow-up
  tasks; bug-follows §4.

## M7: saves and a full play session

- **Task 4, `save-and-reload`.** Waits for the `save` and `load`
  recordings. Then:
  - `psxw run btn --replay save --check card` validates the card the
    session wrote;
  - exit criterion 2 (save-data symbols after loading equal those before
    saving) needs btn's save-data symbols named, e.g. a `[saves]` section
    in `game.toml`, and a generic check over them.

  *From:* M7 task 4.
- **Task 5, `one-day`.** Waits for the recording. *From:* M7 task 5.
- **Task 6, `ramdiff` over `one-day`.** Blocked on D48 (above). *From:* M7
  task 6.
- **Task 8.** Add tasks 4–5 to `psxw ci`; docs; README status. *From:* M7
  task 8.
- **Card conformance and remaining persistence coverage** (exit criterion 3).
  The old missing-device blocker is obsolete: libmcx runs against shared native
  card images or the SIO fallback, and `conform btn --chip memcard` passes nine
  case runs at `99fa333`. Its vblank handler is statically bound and honors BIOS
  patches through existing Flow calls; no Sony BIOS instruction patch is executed.
  libcard's Sony-code patch functions remain native adaptors. These gates do not
  certify every card helper or a game save/reload session; the recordings and
  assertions above are still missing.

  *From:* M7 exit criterion 3; updated by the native BIOS recovery.

## M8: SDK generality

- **Task 5, the translated-SDK cache.** A pinned PsyQ `.LIB` converter in
  Docker, `psxw-lift`'s relocatable mode, the cache in
  `.inputs/sdk-cache/`, and btn linked from it with identical difftest and
  `ramdiff` on `title-to-gameplay` (exit criterion 3). Waits for the
  `.LIB` files. *From:* M8 task 5.
- **Task 6, psyq-decomp C per function.** Parked. psyq-decomp's C names
  data relative to its member (`D_000031F0`, `RO_STR_00003064`,
  `func_0000046C`) in a layout its gitignored build defines. Placing that C
  in a game means mapping those offsets onto the member's section
  placements, which needs that layout. Exit criterion 4. *From:* M8 task 6.
- **Task 8.** Q2 is decided (D41). Docs and README status remain, after
  tasks 5–6. *From:* M8 task 8.

## M9: a second game

**Goal:** run a second decompiled game through the whole pipeline with
only a manifest, replays, and adapters for SDK objects it introduces.
Every M0–M7 oracle passes, `psxw ci btn` still passes, and every code
change is generic: `git diff` over the milestone touches no Rust that
mentions the game (a reviewer agent checks).

- **Choose the game (Q4).** Write a short candidate evaluation from public
  information, then ask the human to pick. Don't pick unilaterally.
  Criteria:
  - a matching decomp with a byte-identical build gate (any coverage);
  - splat layout: symbol_addrs files, auto-names, `target_path`, `sha1`;
  - a PsyQ version other than btn's 4.7, to test object-keyed adapters;
    ideally overlays too;
  - a disc the human can supply, and a PsyQ SDK copy if new objects need
    relocatable members;
  - decomp licensing that allows this use.
- **What bof4 already covers.** M13 runs bof4 through census, lift, build,
  boot and a headless replay. bof4 links PsyQ 4.7 and has no C, so it
  doesn't meet the version criterion. M13 left these to M9: bof4's
  browser host (M5's criteria; the `web-*` gates are skipped in
  `games/bof4/game.toml`), difftest once C exists, saves, a long play
  session, `ramdiff` (D48), mods, and conformance for its objects.
- **Tasks, per game:**
  1. pin the repo in `inputs.lock`; add `games/<id>/game.toml`;
  2. `psxw census <id>`: fix failures generically (normalizer rule,
     splat-convention adapter), each fix with a test that would catch it
     on btn-like code;
  3. `psxw difftest <id>`: zero unexplained mismatches;
  4. `psxw run <id> --check boot` (M2's gates);
  5. the identity report (M8); adapters for new objects, each validated by
     per-object conformance;
  6. replays (a scripted `title-to-gameplay`; a human-recorded long
     session if the human agrees), vectors, goldens, `oracle check
     --device all`, `ramdiff`;
  7. browser determinism (M5's criteria);
  8. a generality report: what the game required, in `docs/design.md`.
- **Exit:** `psxw ci <id>` passes every M0–M7 gate.

*From:* M9 (not started).

## M10: hardware oracle

All of these wait for a rig. Tasks 5 (verdict recomputation, `psxw oracle
disagree`) and 6 (`psxw oracle export`/`import`) are done.

- **Task 1.** The runner's serial transport and serial build variant; the
  host side in `psxw-oracle`. The mailbox protocol (`wire.rs`) is
  transport-independent.
- **Task 2.** Upload paths (Unirom/nops-compatible, FreePSXBoot payload);
  rig setup in `oracle/hardware.md`.
- **Task 3.** Capture the archive, resumable, with progress. Hardware
  goldens go in `hw-…` target directories, which `oracle check` already
  prefers.
- **Task 4, the CD timing envelope.** `CD_SEQ` vectors on a test disc:
  seeks across distances and reads at both speeds, repeated N times. The
  realistic model must fall inside the measured envelope; tune it only if
  it doesn't. Compare btn's loads with the model's figures in M7 Findings
  ("The CD timing envelope is unresolved").
- **BIOS_CALL vectors** against the real BIOS.
- **Task 7.** Hardware-golden verification in `psxw ci` once goldens
  exist; docs; README status.

*From:* M10 tasks 1–4, 7.

**Questions only hardware settles:**
- **XA reserved shifts 13–15.** Beetle shifts by that much; psx-spx says
  they act as 9. psx-wasm follows Beetle (`psx_devices::xa::header`).
  Real encoders never write them. *From:* M10.
- **XA zigzag Table5, taps 24 and 26.** Beetle and the tables' symmetry
  give -2 and 0; psx-spx prints -1 and +1. *From:* M10.
- **GTE `disputed-gte`:** GPL's MAC overflow flag at exactly 2^43−1.
  Beetle flags it; psx-spx and psx-gte don't. *From:* M4 Findings, M10.
- **GTE N/A opcodes 00h and 1Ah,** which Beetle runs as RTPS and DCPL.
  *From:* M4 Findings, M10.
- **GTE behaviours `GTE_EXEC` can't reach:** SXYP, IRGB and FLAG writes.
  *From:* M4 Findings, M10.
- **Provisional device goldens.** `mdec_run`, `cd_cmds` and
  `bios_threads` come from Beetle + OpenBIOS. CD times and the head
  position while not reading are recorded, not compared (D48). *From:* M13
  tasks 13–14.

## M11 and M12: mods

- **Browser runs of M11 acceptance steps 5 and 7** (better-rand linked
  in, and loaded dynamically, in Chromium and Firefox). Waived by the user
  (2026-10-26). Compiled mods have been deprecated since (D56); the
  compiled-module path of the browser loader has never run in a browser.
  Run them only if compiled mods come back. *From:* M11 Status.
- **Overlay data patches miss overlays that arrive by `memcpy` or
  decompression.** Patches apply with the disc's writes (the libcd
  adapter's sectors, `CdGetSector`, BIOS file reads). btn loads overlays
  straight from the disc, so nothing is missed there. Untested on bof4.
  *From:* M11 Findings.
- **Batched memory writes.** A script mod reads and writes memory any
  time, but nothing lands a set of writes atomically between two frames.
  This is not rollback, and changing fields doesn't run the game's
  derived-state updates. *From:* follow-up tasks (M12).
- **Console sessions aren't replayable.** Commands are recorded only as a
  test's `commands`; a session typed at the console isn't. *From:*
  follow-up tasks (M12).
- **Renames between real names.** btn's 587 renames between `3a15992` and
  `b37ae28` all went from a splat auto-name to a real name, so the
  iterations test uses one of those. A census at another decomp commit
  would give a rename between real names. *From:* M12 Findings.
- **Clang-only layouts.** Eight btn headers keep clang's layouts
  (`unchecked` in the snapshot) because GCC 2.95 can't compile them in
  their include context. Giving them the context their C files use would
  let both compilers check them. *From:* M12 Findings.

## M13: bof4

- **Task 5, push the harness.** Needs the human (above). *From:* M13 task
  5.
- **Task 22, `psxw ci bof4` at the current pin.** Every gate passed except
  the replay gate, which ran 600 frames before `psxw run` defaulted to the
  replay's length; the gate's own command passes since. bof4-decomp was
  then repinned to `57f7da1`, and `psxw ci` wasn't rerun. Exit criterion 6
  also wants `psxw ci btn` passing with btn's tiers unchanged (adapter 47,
  c 2523, lift 817). The docs are updated. *From:* M13 task 22.
- **FMV differences against Beetle.** Not gated, not investigated. They
  look like real differences in the FMV path, not D48 timing:
  - **magenta frames** during the intro FMV (counters 771–772, 789–792,
    824–825, 850–852): the display is in 24-bit mode over VRAM that still
    holds 15-bit white. Beetle never shows them;
  - **half pace:** our counter advances once per 8 vblanks during the FMV,
    Beetle's once per 4. Our white screen before it runs longer too (the
    FMV starts at counter 763 vs 476);
  - **a seam at x=303/304:** the rightmost macroblock column of our FMV
    pictures comes from another picture.

  Compare with `psxw oracle ramdiff bof4 --replay title-to-field --at
  V,.. --compare-frames`. *From:* M13 task 21, Findings.
- **A second pad changes bof4's path.** With a pad in port 2 the game
  draws nothing and its loop runs at a fixed rate; with port 2 empty it
  plays normally. Check against Beetle with both ports plugged (replays
  record the pads now). Whether hardware does the same is open. *From:*
  M13 Findings; follow-up tasks.
- **CD-DA.** The controller's Play (03h) plays nothing (`cdc.rs`). bof4's
  disc has two tracks, and Track 2 has never been exercised. If it's
  audio, the CD needs a Play path and SPU CD input. *From:* M13 Context,
  Risks, task 14.

## Debug builds of threaded games (D53, D57)

The DWARF repair in `toolchain/binaryen-dwarf/` (plans
`debug-builds-threaded-games*.md`). Its 2026-10-30 fixes: dead locals
after a resume, statement rows, the Docker builder.
- **Rerun the browser and real-game acceptance.** The README's results
  were recorded with the previous `value-provenance.patch` at `16dafb7`,
  and main has moved on (emulation changes since). Rerun the bof4
  `title-to-field`, btn `title-to-gameplay` and `card-load` release/debug
  comparisons and the three `game-browser.mjs` cases, then update
  `acceptance.json`. *From:* the 2026-10-30 fixes.
- **Release cost of FastISel.** Release builds now compile with FastISel
  and `-g` (D53). Compare bof4's and btn's release `game.wasm` size and
  replay speed with the previous SelectionDAG builds; `wasm-opt -O2`
  should recover most of the selection, unmeasured. *From:* the
  2026-10-30 fixes.
- **The real-game browser checks prove less than their counts.** bof4's
  "24 correct, 8 unavailable": all 8 unavailable are `r29`; 4 of the 24
  are `*(unsigned int*)0x80114a00`, a memory read that uses no variable
  location; 12 are `r16`–`r18`, which FastISel's DWARF gives as
  compile-time constants (`DW_OP_constu`), so the Binaryen repair isn't
  involved. Only the 8 `r2`/`r3` checks go through it, and every check
  expects the same value at every hit, so a wrong location holding that
  value passes. Add checks whose value changes between hits, and a
  variable dead across a switch. *From:* review of the threaded DWARF
  repair, 2026-10-30.
- **Game facts in the toolchain directory.** The browser cases
  `toolchain/binaryen-dwarf/tests/{bof4,btn,bof4-mod}.json` hold disc file
  names, guest addresses, decomp line numbers and pinned SHAs; they belong
  under `games/<id>/`. A comment in `psxw-link/src/dwarf.rs` names a btn
  function (`ClearVramRectAndWait`). *From:* the same review.
- **The browser checks aren't a `psxw` command.** `game-browser.mjs`
  needs Puppeteer, a Chromium and the extension by environment variable,
  and re-implements the host's imports (replay offsets, disc reads) rather
  than using `web/`'s, so it will drift. *From:* the same review.
- **Debug metadata cost.** Location lists now have an entry per original
  opcode. Moddable debug bof4 was 580 MB (527 MB DWARF) and peaked at
  18.5 GB RSS building, with no unpatched baseline to compare. Leads:
  merge entries separated only by Flatten's temporaries; the reader runs
  a second full `dwarf2yaml` just to find stack-location ranges and copies
  every `.debug_*` section; `psxw-link`'s `opcode_ends` keeps a BTreeMap
  entry per output opcode. *From:* the same review.
- **`CodeMap::exclude_expansions` misses same-size rewrites.** Its
  pre-filter (`d - b > c - a`) skips an input instruction rewritten into
  several opcodes that aren't longer in bytes, as when padded relocation
  LEBs compact. The `first_end < end` test after it is exact on its own.
  *From:* the same review.
- **Leftovers in `.worktrees/main-wt-1/.toolchain/`** from the repair's
  research, about 3.3 GB: `llvm-dwarf` (an LLVM source tree, 2.9 GB),
  `binaryen-dwarf-{build,source,reference}`, `native-build-env`,
  `dl-dwarf`, and `binaryen-dwarf-<hash>` builds of earlier patches. Only
  the newest `binaryen-dwarf-<hash>` is used. *From:* the same review.

## Bugs

- **`void` C functions whose callers read a result.** GCC compiles
  `void f(){ g(); }` and `int f(){ return g(); }` identically, so a
  matching decomp can declare `void` a function whose callers read v0. In
  wasm the o32 entry answers v0 = 0. A stopgap is in place: `36764d8` runs
  such functions translated (`build.json` `void_results`: 54 in btn at
  `b37ae28`). It hides a decomp defect by swapping code. Replace it:
  1. **A psxw-cc normalizer rule.** For a C-defined `void` function whose
     original leaves its last callee's v0 at every exit (lift fact
     `returns_callee_v0`) and whose result something reads (a C caller
     declares a result, translated code reads v0 after the call, or its
     address is taken), rewrite the definition before clang sees it: the
     callee's return type, and `return <call>;` at each exit. The census
     counts it like any rule. A flagged function that can't be rewritten
     fails the build, naming the function, the conflicting declarations
     and the evidence. No silent fallback. Then drop the tiering half of
     `36764d8` and keep the detection.
  2. **Fix btn-decompilation.** Each flagged definition returns its
     callee's value, and conflicting prototypes agree; the code still
     matches. Bump the pin. Still `void` at `b37ae28`: the flashback's
     opcode-82 handler `ovl_11_func_800EADB8`, called through
     `gOvl11ObjectOpcodeHandlers`.

  Detection doesn't cover a `void` function reached only by pointer whose
  v0 at return is a temporary rather than a call's result. *From:*
  bug-follows §1; follow-up tasks.
- **Lifted SDK asm that reads temporaries on entry:** `_spu_pitch2note`
  (t1/t2), `func_80037470` and `func_80037494` (t0–t2), `func_8003A2A4`
  (t3). They aren't inlined, so their callers' values don't reach them.
  Audit each. M13 expects more at bof4's scale. *From:* bug-follows §4;
  follow-up tasks; M13 Risks.

### Historical shortcuts-audit findings

Found by the shortcuts audit (2026-10-30; [`shortcuts.md`](shortcuts.md)).
Paths and line numbers below refer to that audit, not current source.
**This is not a current unresolved-bug count.** Recovery has since addressed
ResetEntryInt, closed-event guards, card bounds, old PAD arguments, file-slot
allocation, printf, boot stack selection and several compiler/shared-device
findings; per-call support classes now distinguish partial and unsupported
services without changing the legacy `implemented` build gate. Accepted commits
and tests are recorded in [`salvage-bios-work.md`](salvage-bios-work.md). Other
findings have not all been revalidated; retain them as investigation leads,
not permission for a new project-wide fidelity audit. *Unchecked* records the
original audit's uncertainty.

- **`SetDefaultExitFromException` (B0:18h) does nothing.**
  `psx-bios/src/bios.rs:339` doesn't clear `kernel.custom_exit`, so the
  exception handler keeps entering the old custom exit
  (`psx-runtime/src/driver.rs:51, 85`) after libetc's `StopCallback`.
- **`EnableEvent`/`DisableEvent` change closed event slots.**
  `psx-bios/src/bios.rs:303-316`: enabling a closed handle revives it;
  disabling one leaks it, since `OpenEvent` reuses only unused slots
  (`kernel.rs:70`).
- **Memory card sector 1024 is accepted** as 1023
  (`psx-bios/src/card.rs:178, 186`: `>` should be `>=`).
- **B0:15h and libapi's `PAD_init` disagree on their arguments**
  (`psx-bios/src/bios.rs:332`, `sdk-psyq/src/libapi.rs:105`). psx-spx has
  `OutdatedPadInitAndStart(type, button_dest, …)`.
- **`open` on `cdrom:` hands out descriptor 16 when all are taken**
  (`psx-bios/src/bios.rs:362`); a second overflow replaces the first file.
- **`bios::implemented()` counts calls that always error** (`longjmp`,
  `Exec`, `ReturnFromException`, `SystemError`), which the build's
  coverage check trusts (`psx-bios/src/bios.rs:104`).
- **BIOS `printf` zero-pads negatives as `000-5`** and ignores `+`, space
  and `#` (`psx-bios/src/bios.rs:597-607`).
- **Stale doc on `card_queue`** (`psx-bios/src/card.rs:162`): it always
  returns true.
- **The vblank counter event ignores I_MASK.**
  `psx-bios/src/machine.rs:392` delivers RCntCNT3 every vblank, while
  counters 0–2 are gated on I_MASK (`machine.rs:410`) and libapi's
  `StartRCnt(RCntCNT3)` sets bit 0 expecting the same gate.
- **`GetlocP` loses a minute** (`psx-devices/src/cdc.rs:394`): the pregap
  is subtracted from BCD seconds with no borrow, so 00:58 reads 01:00.
- **GPUSTAT bits 31 and 13 follow a per-vblank field**
  (`psx-devices/src/gpu.rs:516-517`). psx-spx: bit 31 toggles per
  scanline in 240-line modes and is 0 in vblank; bit 13 is 1 when not
  interlaced. *Unchecked* against hardware.
- **GPUSTAT bit 25 is wrong for DMA directions 1–3**
  (`psx-devices/src/gpu.rs:518-520`). psx-spx: 1 = FIFO not full,
  2 = bit 28, 3 = bit 27.
- **Timer 0's dot clock runs 4–10× too fast**
  (`psx-devices/src/timers.rs:31`, `clock.rs:56-58`): it counts the
  53.69 MHz video clock without dividing by the horizontal resolution's
  10/8/7/5/4.
- **Muting or disabling the SPU silences CD audio**
  (`psx-spu/src/lib.rs:722-724`). psx-spx SPUCNT bits 14–15: "Don't care
  for CD Audio".
- **SPU manual-transfer writes past 32 are dropped**
  (`psx-spu/src/lib.rs:349-354`); later DATA writes wait for the next
  SPUCNT write.
- **15-bit colour is shifted, not expanded, at presentation**
  (`psx-render-gl/src/shaders.rs:341-344`): white shows as F8F8F8.
  *Unchecked.*
- **libgpu's timeout counts every vblank** when libetc's adapters aren't
  running (`sdk-psyq/src/libgpu.rs:324-328`); the original calls
  `VSync(-1)`.
- **libmcx's last-byte wait is ~20 cycles, not a byte's ~1,000**
  (`sdk-psyq/src/libmc.rs:498-505`). The comment predates D66; probably
  a regression from it.
- **A tail transfer out of an inlined callee exits the whole caller**
  (`psxw-lift/src/graph.rs:231-237, 260-264`; `emit.rs:198-201, 255-258,
  306-314`). *Unchecked.*
- **`bltzal`/`bgezal` with rs = ra test the new link value**
  (`psxw-lift/src/emit.rs:213-234`). Restricted on MIPS; GCC doesn't emit
  it.
- **`psxw_lift::bios_calls()` recognises only `addiu t1`**
  (`lib.rs:524-528`), not `ori` as `t1_after_slot` does, so an `ori t1`
  ChangeThread trampoline misses the Asyncify scope
  (`psxw-build/src/game.rs:527`). *Unchecked.*
- **GTE operands parse shifts tighter than `+`**, the reverse of C
  (`psxw-cc/src/gte.rs:236-259`).
- **The EXE header's stack beats `SYSTEM.CNF`'s `STACK=`**
  (`psx-runtime/src/boot.rs:68`). On a real BIOS `LoadExec` writes
  `STACK` into the header (from memory, *unchecked*).

## Gaps found along the way

**Browser**
- **"WebGL2 is not available" in a real browser** after a test run
  rebuilt the served bundle mid-session. The rebuilt files were
  byte-identical, so it was most likely the browser blocking WebGL after a
  GPU-process reset; a full browser restart clears it. If it recurs, the
  page could say so (restart, or check `chrome://gpu` / `about:support`).
  *From:* follow-up tasks.
- **WebGL context loss in the real page.** With JSPI the game yields to
  the Worker's event loop every vblank (`9fa7bfc`), so the
  `webglcontextlost`/`restored` listeners can run. Recovery in the page
  itself is untested; `web-test gpu` uses its own canvas. *From:*
  follow-up tasks.
- **Browsers without JSPI** (`WebAssembly.Suspending`) fall back to the
  never-yielding Worker. Chromium leaks a frame a vblank there, and dev
  mode always does (its JS trampolines can't be suspended). The test
  image's Chromium and Firefox both have JSPI. *From:* follow-up tasks.
- **Lag during loads.** Over a 60 s idle run, 495 (Chromium) and 635
  (Firefox) frames ended more than a period late, with frame-time peaks of
  128 and 172 ms. The run still ends within one frame of real time. Find
  where the time goes: OPFS reads, renderer readbacks, or the CD model's
  bursts. *From:* follow-up tasks.

**Oracle recording**
- **GTE recording.** `GteHook` exists but no game run calls it. It needs a
  host import like the GPU and SPU hooks (ABI 6). *From:* follow-up tasks.
- **GPU recording.** Only linked-list transfers (`DrawOTag`) are
  recorded, not port sequences (`DrawPrim`) or image transfers. Batches
  with a command over 16 words are skipped until the runner paces within a
  node. *From:* follow-up tasks.
- **SPU recording yield.** 59 of 80 windows were timing-sensitive, mostly
  because of the ~270 state writes at sample 0. Spreading those writes, or
  starting windows at quiet moments, would keep more. *From:* follow-up
  tasks.

**Elsewhere**
- **UBSan ignores.** The three `[ubsan.ignore]` entries in
  `games/btn/game.toml` (`StartSoundArchiveRead`, `StepSoundLoad`,
  `BeginSoundBankBodyTransfer`) trace to the decomp declaring
  `extern s32 gSoundArchiveOffsets[3]`, a table of 151 entries. Fix the
  header upstream, bump the pin, drop the entries. *From:* follow-up
  tasks.
- **"GPU timeout" at frame 99** in every btn run: libgpu's `DrawSync`
  timeout during boot, probably the game's own doing (`startIntr` writes
  DPCR). Confirm against the emulator. *From:* follow-up tasks.
- **The Tauri shell** (M5 task 7). Declined for now (D46). *From:* M5;
  follow-up tasks.

**Stale code comments** (found while consolidating the plans):
- `psx-devices/src/lib.rs` says nothing in the crate is a register-level
  model and that the GPU, SPU and SIO "arrive with their milestones"; the
  crate holds register-level models of all of them (D40–D58).
- `psx-devices/src/irq.rs` and `sio.rs` say they serve conformance runs
  only; every game runs over them.
- `psxw-abi` `phys::IO` says debug builds trap there; only `--debug-io`
  builds do, and device-library accesses reach the bus. `RUNTIME_PRIVATE`
  says it holds the safepoint budget, which is a wasm global.
- The runtime ignores `BootOptions::safepoint_budget`; the budget is set
  at link time (`psxw-build`, 1,000).
- D57 says Asyncify instruments about 13% of functions; M13's Findings
  still give the older 21% figure (3,693 of 17,774 in bof4).

*From:* the plan consolidation (2026-10-29).

## Tests that still embed btn facts

Found by M13's generality review. Replace each with made-up values.
- btn's serial in card tests: `psx-devices::memcard`
  (`BASLUS-01115BTN01`) and `psx-bios::card` (`bu00:BASLUS-01115TEST`).
- btn symbols and addresses in the `psxw-cc::asm`, `psxw-inputs::map` and
  `psxw-modkit::decl` tests.
- `psxw build btn` and `psxw run btn` in the CLI help examples
  (`psxw-cli/src/help.rs`).

*From:* M13 Findings (generality review); follow-up tasks.
