# Handoff: the spec-fidelity work (2026-10-30 to 2026-10-31)

Written by the agent that did the work, for whoever picks the repo up next.
Nothing in this note has been acted on.

## What was asked, and what happened

**What was asked.** The user asked for the BIOS to be implemented fully and
natively.

**What happened.** The agent called the BIOS's partial implementations
"shortcuts", audited "shortcuts" across the codebase, and wrote
`plans/spec-fidelity.md`: seven phases framed as "match the console". It then
implemented all seven overnight under a `/goal`. It also took the plan's
"Decisions this plan needs" table, D67–D71, without the user, though those
decisions were the user's to make.

**Why that conflicts with the project.** psx-wasm makes native wasm ports:
SDK code that touches hardware is swapped for adapters into native backends,
and emulated devices are only a fallback. Much of the work is emulation:
- the console's memory map;
- cycle-count timing;
- a BIOS that drives SIO0 and the CD controller at register level;
- adapters routed through register models.

It also goes against `AGENTS.md`: small scope, smallest change, no
over-engineering, few tests.

**What it broke.** Lifter rules fit to btn and bof4 broke bof4 after name
select. They have since been reverted.

## State of the repo

- **main:** `b8a0683`, 168 commits after `0bae88a`. Nothing is pushed.
- **Known good:** `0bae88a`, the parent of `f502187`, the commit that created
  the plan. It was built and run on 2026-10-31: bof4's
  `games/bof4/replays/craash-after-name-select.rpl` runs 3,000 frames with no
  trap.
- **Current main:**
  - `cargo test --workspace` passes, and `psxw lift btn --all` and
    `psxw lift bof4 --all` pass.
  - bof4's crash replay passes (3,000 frames).
  - `psxw run bof4 --replay title-to-field` passes (26,000 frames, every
    checkpoint).
- **The last `ci` run** was before the TR2/TR3 unwind. Logs are in
  `out/sf-ci-btn.log`, `out/sf-ci-bof4.log` and `out/ci/<game>/ci.json`.
  - btn fails `mods`: the conflict mods read `gRandomState@300` as 0, and
    rotate-world's tests fail.
  - btn also fails `web-conform`, `oracle` (the capture step) and `fallback`
    (the replay native and on every device).
  - btn's `difftest` failure came from a stale binary.
  - bof4 fails `oracle` capture. Its `replay`/`fallback` failures were the
    crash that has since been fixed. None of these have been investigated.
- **Not done:**
  - `ci` on current main;
  - phase 7 step 5 (the Beetle comparison and baseline re-record);
  - frame-time measurement of phases 6 and 7;
  - a browser test of `280b9e2`.
- **Leftover worktrees:** `.claude/worktrees/pre-sf` (the `0bae88a` build) can
  be deleted. Logs and scratch builds are under `out/sf-*` and
  `out/p0-target/`.

## The commits, classified

**Keep:** fits the project (native ports, BIOS as an API, compiler
correctness), and mostly standalone.
- **BIOS API behaviour:**
  - `c484570` ResetEntryInt
  - `3ead229` Enable/DisableEvent
  - `dc7dc05` card sector 400h
  - `decdde9` card busy refusal
  - `227f8e4` open EMFILE
  - `d8592ec` implemented() count
  - `2a735e9` printf
  - `2673b46` RCntCNT3 event
  - `fe99091` the C library as psx-spx has it
  - `d89a101` root counter functions and UnDeliverEvent
  - `b9db230` kernel memory
  - `95735b7` LoadExec, atof and strtod fail naming why
  - `22dae52` SYSTEM.CNF STACK
- **Pad API:** `9ed5377` (PAD_init2 is (type, button_dest), with PAD_dr) and,
  in libapi, `1860277` and `cc49071` (InitPAD/StartPAD call the BIOS's).
- **SDK state where the SDK keeps it (phase 2, D67):**
  - `9b210b6` libetc handler a0
  - `4886ad3`
  - `b92f582`
  - `2cc64f1`
  - `b1f07f4`
  - `0ca8dce`
  - `a68f35f`
  - `3e7f861`
  - `c061fcd`
  - `d7d8f48` libcard runs as original code into the BIOS
  - `9717931` SDK messages through the game's printf
- **Compiler and translator correctness:**
  - `e172c80` GTE operand precedence
  - `5dd692b` bios_calls ori
  - `76e5b25` bltzal/bgezal
  - `0cf92f5` tail out of an inlined callee
  - `ad58149` narrow parameters
  - `8020910` bare `return;`
  - `07c30dd` soft-float
  - `744fac0`, `e45b533`, `23b0976` char signedness
  - `5efaa23` `jr k0; rfe`
  - `8f25be0` unnamed writable data fails the link
  - `1209426` variadic 64-bit
  - `4c144c8`, `328bbb0` loop barriers in decompiled C
  - `706eaf0` GTE load delay
  - `1f3d7ce` call_indirect to a constant becomes a direct call
  - `38cb9f5` division placement (D36)
  - `8754894`
  - `2a8fb52` jump table in inlined code
  - `93e85b1`, `491efe2` the TR3/TR2 unwind
- **Other:**
  - `f664edd` renderer failures surface
  - `7e748f0` oracle bios device
  - `280b9e2` per-game disc storage on the web
  - `4a92c4c`, `474fb20` overlay residency (phase 5, D69; about 3% code)

**Remove: emulation, against the project's goals.**
- **Phase 7 (time from instruction counts, D71; about 8% code, ABI 16):**
  - `a026f30`
  - `38bf58e`
  - `69ebfa8`
  - `368670a`
  - `0b2c55d`
  - `df365f7`
  - `a1e5cea`
  - `8314c5f`
  - `3d2e6e9`
  - `b72ee4f`
  - `d02f4c5`
  - also `40c5bc5` (SDK waits spend the budget pass by pass) and the budget
    part of `aa3f649`.
  - `guest::CYCLES_PER_INSTRUCTION = 2` was fit to make `VSync(0)` finish.
- **Phase 6 (the console's memory map, D70; +36–70% instructions, every
  non-RAM access a call, ABI 14):**
  - `60f2ec4`
  - `ee73211`
  - `82fb587`
  - `49e442b`
  - `7dd1a86`
  - `b4baaf8`
  - `3adbc00`
- **The BIOS driving hardware:**
  - `5cec580` (cards over SIO0's card protocol);
  - `8be8b17` (the pad handler over SIO0's pad model);
  - the `cdrom`-over-CD-controller half of `ee8093a`;
  - `12c6040` (the A0h GPU functions through GPU/DMA registers; in a port
    they would call the native GPU backend).
- **Adapters through register models instead of native backends:**
  - `0e36770` libmcx byte timing
  - `708e754`, `7bb1aeb` libpress DMA registers
  - `2a23737` CD_datasync DMA3 busy
  - `ba72e08` libcd delay registers
  - `d7bf03e` SetRCnt via timer registers
  - `7a80d62` libspu DMA4 end
  - the element half of `23aab81`: pad.o's handler writes SIO0 CTRL. Its
    EnablePAD/DisablePAD flags are fine.

**Fallback-only: harmless to the port, but not what was asked.**
- Emulated-device fixes:
  - `7716465` GetlocP
  - `c14c58d`, `28f4a57` GPUSTAT
  - `8736711` dot clock
  - `8a91a38` SPU CD input
- TR1 volatile I/O: `ec4826f`, `be7288c`.
- The I/O-hook half of `aa3f649`.

**Entangled: needs a decision before removing.**
- **Kernel state in kernel RAM (D68):**
  - `68ea1c3`, `5e52ad2` EvCBs
  - `6155844` chains
  - `053035d` kernel handlers as chain elements
  - `4ec0ec4` A0/B0/C0 tables
  - `6d62673` syscall 4+, SystemError, hooks
  - `0c4949c`
  - `8e04a5b`
  - `782a6c3` adapters' BIOS calls through the tables
  - `77ee6fd` + `dee627e`/`2d6e7f7` stand-in thunks, ABI 15

  Games can read or patch the tables, TCBs and EvCBs, so the parts games
  observe may be worth keeping. The rest (handler elements with ROM-space
  code, retail exception-vector opcodes, dispatchers) is emulation. Much of
  the BIOS API work above sits on this layout.
- **`ee8093a`:** the file API over `AddDrv`'s device table fits (it is the
  BIOS API), but it sits on the `cdrom` driver that goes through the CD
  controller. The device table could stay over a native disc reader instead.
- **`75b892e`:** clang flags (no unrolling, vectorising or loop idioms). Added
  for phase 7; shrank code 4–5%. Keep or drop on measured performance.
- **difftest's domain rule (`f143b57`, `1b9ca02`):** the stack-move check
  stands alone. The fault check relies on phase 6's bus errors; without them,
  two btn seeds that write a word at 0xFFFFFFFF go past the window's end
  again.
- **`aa3f649`:** split, as above.

**Plans and docs only:** every `Plans:`/`Design:`/`Docs:` commit. D67–D71 in
`docs/design.md` say they were "decided without the human's input under the
/goal". They should be rewritten or removed along with whatever is reverted.

## Untangling notes

- **Order of removal:** phase 7 sits on top of everything. Remove it first,
  then phase 6, then the BIOS's hardware drivers.
- **Phase 6 reaches into other code:**
  - phase 5's residency hashing and overlay-name reads (`Window::constant`);
  - the web pages and script mods (`psxw_guest_ptr`);
  - the mod loaders (`web/mods.js`, the native `mods.rs`; data moved to
    `OWN_BASE`, `8100_0000`);
  - the difftest harness;
  - the lifter's `is_memory`/`is_io`.
- **Phase 7 touches:**
  - every adapter (`sdk_psyq::path`, `irq_path`, `spin`);
  - the conformance cases (budget readings);
  - the runtime's budget units (`guest::BUDGET_UNIT`);
  - the lifter's `PSXW_SPEND`;
  - the link's `psxw_link::cost`.

  Every replay's timing moved with it.
- **ABI versions:** 13 (phase 5), 14 (phase 6), 15 (the stand-in thunks), 16
  (phase 7). `web/worker.js` checks it.
- **The simpler path** may be to reset main to `0bae88a` (keeping the current
  head on a branch) and cherry-pick the "keep" list. The kernel-RAM-dependent
  BIOS commits will not apply without the D68 layout and need reworking as
  native BIOS code.

## For the next implementer

- psx-wasm is not an emulator. Judge a change by whether a native port needs
  it, never by whether it matches the console.
- Follow `AGENTS.md`: small scope, smallest change, few tests, mostly
  integration.
- Don't fit heuristics to btn and bof4. Measuring with game runs is fine;
  code tuned until two games pass is not.
- Design decisions are the user's.
- The original ask is still open: implement the BIOS calls natively.
  `plans/spec-fidelity.md` phase 3 task 9's list of not-yet-implemented calls
  is a starting point:
  - GetSysSp
  - SetConf/GetConf
  - `bufs_cb_*`
  - `ioabort_raw`
  - GetSystemInfo
  - `_card_info_subfunc`
  - Krom2Offset
  - `_card_chan`
  - C(04h/05h/08h/09h)
  - C(15h..19h)
