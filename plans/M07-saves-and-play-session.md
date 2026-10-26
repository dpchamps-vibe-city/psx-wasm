# M7 — Saves and a full play session

**Status:** in progress (tasks 1–2 and the native half of 3)
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

1. [x] Memory card device and image format; `.mcr` import and export.
   *(`psx_devices::memcard`: raw 128 KiB images are both the import and
   the export format.)*
2. [x] Adapters plus conformance (SIO card decoder). *(D43: libmcrd runs
   as guest code over the HLE BIOS card functions, with no adapters;
   `psxw conform btn --lib libmcrd` checks it end to end.)*
3. [ ] Host storage: native and OPFS. *(Native done: `card0.mcr` and
   `card1.mcr` in the run's storage directory. OPFS comes with M5.)*
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

- **2026-10-26:** memory cards (D43).
  - **The card model:** `psx_devices::memcard`, per psx-spx's data format.
    It has the header and directory frames with XOR checksums, block
    chains, create/delete/undelete/rename/format, and wildcards.
  - **The BIOS card subsystem:** `psx-bios::card`.
    - `_card_read`/`_card_write`/`_card_info`/`_card_load`/
      `card_write_test` complete one per vblank and deliver F0000011h and
      F4000001h events. The new-card flag and `_new_card` work.
    - The `buXY:` file functions: open/create, lseek, sync and async
      read/write (async returns 0 when accepted), close, firstfile/nextfile,
      erase, undelete, rename, format, errno. The A0h aliases of the file
      functions are covered.
    - Images load from host storage (slot 1 is formatted if absent) and
      are written back at vblank.
  - **Adapters:** libmcrd runs as guest code (boundary `guest`). libcard's
    BIOS wrappers keep thin adapters, since `_patch_card` scans Sony BIOS
    RAM. libapi's `firstfile` adapter now calls the BIOS's `firstfile2`.
  - **`psxw conform btn --lib libmcrd` passes** (19 steps):
    - Exist reports a new card (3) in slot 1 and no card (1) in slot 2;
      Accept takes slot 1;
    - CreateFile and Open succeed (0); a 128-byte WriteData and ReadData
      at offset 100h finish with 0 after 7 vblanks each, and the data
      reads back exactly;
    - GetDirentry lists one file: `BASLUS-00000PSXW`, 2000h bytes, first
      sector 40h.
  - **What it found:**
    - libmcrd retries async `write` until it returns 0;
    - `MemCardGetDirentry` goes through libapi's `firstfile`;
    - the conformance driver needed stack arguments
      (`Conform::call_args`).

## Findings
