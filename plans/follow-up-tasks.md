# Follow-up tasks

What's left after M0–M6 (now in [`completed/`](completed/)), as of
2026-10-26. `psxw ci btn` passes all 15 gates. btn boots from the disc and
plays from the title to the farm, natively and in Chromium and Firefox,
with the same RAM, VRAM and audio on all three.

## Check first

The browser display fix (`282b413`) passes the headless check:
`web-test game btn --replay title-to-gameplay --frames 1500` showed 1496
(Chromium) and 1500 (Firefox) frames, 37 of 50 sampled lit. `web-test game`
and `web-test pacing` now build into `out/web/test-<game>/`, so they never
rewrite the `out/web/<game>/` bundle someone is serving.

- **"WebGL2 is not available" in a real browser** after a test run rebuilt
  the served bundle mid-session. The rebuilt files were byte-identical, so
  it was most likely the browser blocking WebGL after a GPU-process reset;
  a full browser restart should clear it. If it recurs, the page could say
  so (restart, or check `chrome://gpu` / `about:support`).

- **WebGL context loss in the real page.** With JSPI the game yields to
  the Worker's event loop every vblank (`9fa7bfc`), so the
  `webglcontextlost`/`restored` listeners can run now; recovery in the
  page itself is still untested (`web-test gpu` uses its own canvas).
- **Browsers without JSPI** (`WebAssembly.Suspending`) fall back to the
  never-yielding Worker: Chromium leaks a frame a vblank there, and dev
  mode always does (its JS trampolines can't be suspended). Chromium and
  Firefox in the test image both have JSPI.

## Needs the human

- **M7 task 4, `save-and-reload`:** record two sessions in the page
  (`web/README.md`): `save` plays until the game saves, `load` boots and
  loads that save. Store them as `games/btn/replays/save.rpl` and
  `load.rpl`. Then:
  - `psxw run btn --replay save --check card` validates the card the
    session wrote;
  - the "expected save contents" assertion still needs its save-data
    symbols named, e.g. a `[saves]` section in `game.toml`.

  Scripting these sessions from frame dumps was tried and dropped.
- **M7 task 5, `one-day`:** record it the same way.
- **M7 task 8:** `psxw ci`, docs and README status once 4–5 land.
- **The pacing gate:** add `machine = "…"` to `psxw.local.toml` to make
  `web-pacing` gate rather than report.
- **M8 tasks 5–6 (deferred):** the PsyQ `.LIB` files. Task 6 also needs
  the member layout they carry.
- **M9:** a second game, its decomp and disc.
- **M10:** a capture rig.

## Blocked on CD timing (D48)

M4 task 11, M6 task 10 and M7 task 6 all ask `ramdiff` for zero
differences.
- **Why they can't get there:** btn steps its RNG on every pass of its
  loading loop, so state after the first load depends on load time. The
  two drive models disagree on load time, and Beetle adds random jitter
  to each CD command.
- **Ruled out:** patching Beetle to record its CD events (the human's
  call).
- **Routes if revisited:**
  - move libcd to original code over a register-level CD controller, then
    find an unpatched way to share the emulator's timing;
  - or settle for equality only up to the first load.
- **Other difference classes ramdiff shows** (M4 Findings), which would
  need a design or justified exclusions:
  - libcard's BIOS-dependent self-patching (`_patch_card*`);
  - libetc's interrupt stack inside `Hcount`'s map range;
  - libcd's internal state, which stays in the runtime under adapters;
  - `Vcount`.

## Gaps found along the way

- **Interactions after the opening (talk, pick up, collide with NPCs,
  the dog, weeds, stones):** reported broken while the opening event was
  skipped. Recheck in the page now that it plays.
- **`void` C that returns a value.** GCC compiles `void f(){ g(); }` and
  `int f(){ return g(); }` identically, so a matching decomp can declare
  `void` a function whose callers read its v0: btn's arrival-event
  request (a `void` wrapper around an allocator) and the flashback's
  opcode-82 handler (through the original's opcode table). The build now
  translates such functions (`build.json` `void_results`, 62 in btn):
  C callers declaring a result, translated callers reading v0, and
  address-taken functions whose every exit leaves a callee's v0. Not
  covered: a `void` function reached only by pointer whose v0 at return
  is a temporary rather than a call's result; and the decomp's own
  declarations, which upstream could fix.
- **Emulator title-menu timing:** the menu appears about 10 counter steps
  later on the emulator than natively, so a press recorded just after it
  appears natively (`start-no-cutscene-interaction`, UP at 286) is lost
  there. Loads also tick the counter differently (D48).
- **Lifted SDK asm that reads temporaries on entry** (`_spu_pitch2note`
  t1/t2, `func_80037470`/`94` t0–t2, `func_8003A2A4` t3): not inlined,
  so their callers' values don't reach them. Audit each.

- **Browser lag during loads.** Over a 60 s idle run, 495 (Chromium) and
  635 (Firefox) frames ended more than a period late, with frame-time
  peaks of 128 and 172 ms. The run still ends within one frame of real
  time. Find where the time goes: OPFS reads, renderer readbacks, or the
  CD model's bursts.
- **GTE recording.** `GteHook` exists but no game run calls it. It needs a
  host import like the GPU and SPU hooks (ABI 6).
- **GPU recording:**
  - only linked-list transfers (`DrawOTag`) are recorded, not port
    sequences (`DrawPrim`) or image transfers;
  - batches with a command over 16 words are skipped until the runner
    paces within a node.
- **SPU recording yield.** 59 of 80 windows were timing-sensitive, mostly
  because of the ~270 state writes at sample 0. Spreading those writes or
  starting windows at quiet moments would keep more.
- **UBSan ignores.** The three functions in `[ubsan.ignore]` all trace to
  the decomp declaring `extern s32 gSoundArchiveOffsets[3]`, a table of
  151 entries. Fix the header upstream, then drop the entries.
- **libmcx.** The adapters answer McxErrInvalid for an ordinary card, from
  btn's own handling of the result. Conformance against the original
  library would need an SIO memory card device. It would also need the
  BIOS exception-handler patching libmcx's fast DSR does, which the HLE
  BIOS has nothing for; that's why it keeps adapters.
- **"GPU timeout" logged at frame 99** in every btn run: libgpu's
  `DrawSync` timeout during boot, probably the game's own doing (`startIntr`
  writes DPCR). Confirm against the emulator.
- **M5 task 7, the Tauri shell:** declined for now (D46).
- **Housekeeping:** `.claude/worktrees/` (untracked) is left over from
  agent runs and can be deleted.
