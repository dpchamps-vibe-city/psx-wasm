# Open items

Every open task, blocked item, human-needed input, unresolved finding and
erratum, as of 2026-10-29. Milestone history is in [`completed/`](completed/).
Each item says what it is, why it's open, and where it came from.

- **Last full gate runs:** `psxw ci btn` passed every gate on 2026-10-27
  (M12). `psxw ci bof4` hasn't run at the current pin (M13 task 22).
- **The hardware boundary** (native adapters vs emulated devices) is
  planned in [`unified-adaptor-layer/`](unified-adaptor-layer/).

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
- **libcard and libmcx conformance** (exit criterion 3). `psxw conform`
  covers libpad, libgpu, libmcrd, libetc, libsnd and libspu.
  - **libcard** keeps thin adapters: `_patch_card` scans Sony BIOS RAM,
    which doesn't exist here.
  - **libmcx's** adapters answer McxErrInvalid for an ordinary card, taken
    from btn's own handling of the result. Conformance against the
    original library needs an SIO memory card device, and the BIOS
    exception-handler patching libmcx's fast DSR does, which the HLE BIOS
    lacks.

  *From:* M7 exit criterion 3; follow-up tasks.

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
  and `docs/status-btn-vs-bof4.md` say 21% (3,693 of 17,774 in bof4).

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
