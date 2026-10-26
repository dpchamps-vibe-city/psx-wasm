# Follow-up tasks

What's left after M0–M6 (now in [`completed/`](completed/)), as of
2026-10-26. `psxw ci btn` passes all 15 gates. btn boots from the disc and
plays from the title to the farm, natively and in Chromium and Firefox,
with the same RAM, VRAM and audio on all three.

## Check first

- **The browser display fix** (`282b413`). The Worker now posts each frame
  to the page as an ImageBitmap; before that the visible canvas stayed
  black. It is confirmed by hand in a real browser, but not yet by the new
  headless check (`web-test game` requires a lit frame among those the
  page showed). Run:

  ```sh
  psxw web-test game btn --replay title-to-gameplay --frames 1500 --hash 600,1500
  ```

  The last attempt failed at `docker run` with a usage error, before any
  browser started, and wasn't investigated.
- **WebGL context loss in the real page.** The Worker now owns its
  `OffscreenCanvas`. Its `webglcontextlost`/`restored` listeners only run
  when the Worker returns to its event loop, which the game never lets it
  do. `web-test gpu` covers recovery on its own canvas, not in the page.

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
