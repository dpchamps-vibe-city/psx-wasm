# M7 — Saves and a full play session

**Status:** not started
**Depends on:** M4, M6 (and M5 for browser storage). **Unlocks:** M9.
**Design refs:** design: Runtime (sio), Oracles (whole-game); decisions
D21–D23.

## Goal

The game saves and loads like on the console, and a long session of real
play matches the emulator symbol for symbol. This is the end-to-end
oracle for the whole system.

## Context

- **Memory card libraries btn calls:** libmcrd (`MemCardInit`, `Start`,
  `Accept`, `Exist`, `ReadFile`, `WriteFile`, `GetDirentry`, `Format`,
  `Sync`), libcard (`InitCARD`, `StartCARD`), and libmcx (the PocketStation
  library: `McxCardType`, `McxExecApl`, …).
- **btn's save code** (in overlay 15) computes and verifies its own save
  checksums. That's an oracle we get for free.
- **The configuration is part of device behaviour:** cards per slot,
  formatted or not, PocketStation present or absent. They're runtime
  options with game defaults in `game.toml`.

## Scope

**In**
- **Memory card device (`psx-devices::sio`):**
  - 128 KiB images: 16 blocks of 8 KiB; 128-byte frames;
  - the directory frames with their XOR checksums;
  - card states (absent, unformatted, formatted, full);
  - the read and write frame protocol at the adapter level, with
    write-timing semantics on the virtual clock.
- **Adapters:**
  - libmcrd, libcard, and the BIOS card functions if used, by the M1
    hardware-access classification;
  - libmcx reporting "no PocketStation" by default, as a configuration,
    not a gap.
- **Persistence** through `host.storage_*`: native files; OPFS in the
  browser. Import and export of standard `.mcr` images.
- **Conformance** of the adapters against the translated originals,
  through a test-only SIO memory-card protocol decoder.
- **Replays:**
  - `save-and-reload` (scripted): reach a save point, save, reset, load;
  - `one-day` (recorded by the human playing the native or browser
    build): one full in-game day.
- **The long-session oracle:** `ramdiff` over `one-day` with no
  exclusions.

**Out:** PocketStation emulation (unsupported by design; reported as
absent).

## Tasks

1. [ ] Memory card device and image format; `.mcr` import and export.
2. [ ] Adapters plus conformance (SIO card decoder).
3. [ ] Host storage: native and OPFS.
4. [ ] `save-and-reload` replay; checks: the frame checksums are valid,
   and the game reports a successful load with the expected save
   contents, as an assertion on save-data symbols.
5. [ ] Ask the human to record `one-day`. Store it in
   `games/btn/replays/`.
6. [ ] `psxw oracle ramdiff btn --replay one-day`: drive it to zero
   differences, fixing devices and adapters (never adding exclusions).
7. [ ] Document the CD timing envelope as unresolved (hardware only), with
   evidence of how long loads take under realistic timing, for M10 to
   compare against.
8. [ ] Add everything to `psxw ci`; docs, README status.

## Exit criteria (all automated)

1. Every directory and frame checksum written by the game validates, and
   images round-trip through `.mcr` export and import byte for byte.
2. `save-and-reload`: the game accepts its own save. The save-data symbols
   after loading equal those before saving.
3. Adapter conformance passes for libmcrd, libcard and libmcx.
4. `psxw oracle ramdiff btn --replay one-day`: zero differing symbols at
   every aligned point, with no exclusions except the stack.
5. `psxw ci btn` includes 1–4, plus all earlier milestones' gates.

## Risks and decision points

- **The emulator's memory card timing** may differ from ours. Alignment by
  the game's frame counter absorbs it. If the game's logic itself depends
  on card timing, record it as a Finding.
- **The human needs to record `one-day`.** It's the only human-recorded
  input this plan requires.

## Progress log

## Findings
