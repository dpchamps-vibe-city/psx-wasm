# Shortcuts

Every place the runtime, the BIOS, the devices, the SDK adapters or the
translator behaves differently from the hardware or the original code
without failing loudly, as found by an audit on 2026-10-30. Each item is a
task: fix it to the spec, then tick it with the commit.

## What counts

A **shortcut** is behaviour that silently deviates from what a PS1 or the
original SDK code does: a hard-coded value, an ignored flag or mode, timing
collapsed to instant, a status that is always ready, a skipped state, an
error swallowed into a default, or one component reading another's
internals instead of its interface.

Not shortcuts, so not listed:
- the architecture itself: static recompilation, adapters in place of SDK
  code, register-level devices;
- gaps that fail loudly (an error, trap or failed build naming what's
  unsupported). That is the project's policy for gaps. The one exception
  is the BIOS call tables (BIOS0 below), tracked because the BIOS is to be
  completed.

Differences the architecture forces are recorded under [Forced](#forced),
not as tasks.

## How to work an item

- **The spec decides what's right:** psx-spx, OpenBIOS's source, Beetle
  through the oracle, the original SDK object. Never what a current target
  game happens to need. Game runs are regression checks only. A run that
  changes after a fix points at a bug in the fix, or at an adapter written
  against the old shortcut; fix that to the spec too.
- One item per commit where practical. Tick it and add the commit hash.
- If an item turns out to be forced by the architecture, move it to
  [Forced](#forced) with the reason.
- If a fix changes a recorded decision (a D-number), update
  `docs/design.md` in the same commit.

Each item gives the location (paths relative to `crates/`), what the code
does, what the spec says, then: **Ref** (the decision or open item that
documents it, or *undocumented*), **Impact** (could a game other than the
current targets notice: low, med, high), **Checked** (*yes* when the claim
was re-read against the code after the audit, *audit* when it rests on the
audit's reading alone).

## Counts

| Area | Items | Documented | Undocumented |
|---|---|---|---|
| [BIOS](#bios) | 23 | 4 | 19 |
| [Emulated devices](#emulated-devices) | 42 | 10 | 32 |
| [SPU, GTE, renderer](#spu-gte-renderer) | 10 | 3 | 7 |
| [SDK adapters](#sdk-adapters) | 22 | 18 | 4 |
| [Translator and runtime](#translator-and-runtime) | 24 | 8 | 16 |
| **Total** | **121** | **43** | **78** |

## Start here

Plain bugs, each a small fix: BIOS1, BIOS2, BIOS3, BIOS4, DEV3 (GetlocP),
DEV30 (timer 0 dot clock), SPU1 (mute silences CD), SPU2 (FIFO overflow),
SDK13 (libmcx last byte), TR19 (GTE operand precedence).

Highest impact:
- TR8, overlay residency checked by two code words;
- TR9, virtual time counted in loop passes;
- TR1, lifted I/O accesses aren't volatile;
- TR21, COP0 status register writes ignored;
- TR10, every address masked to 24 bits;
- TR2, registers outside o32 arrive as 0;
- DEV1, CD-DA plays nothing;
- DEV19 and SDK1, digital pad only;
- SDK4 and SDK2, SDK state kept outside guest RAM with no build check for
  other readers; libetc's justification rests partly on what the current
  target games read.

## BIOS

### Bugs

- [ ] **BIOS1. `SetDefaultExitFromException` (B0:18h) does nothing.**
  `psx-bios/src/bios.rs:339`. Returns 0 without clearing
  `kernel.custom_exit`, so the exception handler keeps entering the old
  custom exit (`psx-runtime/src/driver.rs:51, 85`). libetc's
  `StopCallback` relies on it. Spec: restores the default exit.
  Ref: undocumented. Impact: med. Checked: yes.
- [ ] **BIOS2. `EnableEvent`/`DisableEvent` change closed event slots.**
  `psx-bios/src/bios.rs:303-316`. `kernel.ev()` returns unused slots too:
  `EnableEvent` on a closed handle revives it with its old class and
  callback; `DisableEvent` marks it disabled, and `OpenEvent` never reuses
  it (`psx-bios/src/kernel.rs:70`). Spec: only a slot in use changes.
  Ref: undocumented. Impact: low-med. Checked: yes.
- [ ] **BIOS3. Memory card sector off by one.** `psx-bios/src/card.rs:178,
  186`. `sector > FRAMES` lets sector 1024 through and clamps it to 1023,
  so a write to the invalid sector overwrites the card's last one.
  Spec: sectors 0–3FFh, else fail. Ref: undocumented. Impact: low.
  Checked: yes.
- [ ] **BIOS4. B0:15h disagrees with libapi's `PAD_init` on its
  arguments.** `psx-bios/src/bios.rs:332` treats a0 and a1 as two pad
  buffers; `sdk-psyq/src/libapi.rs:105` treats a0 as the type and puts
  both ports at a1. Spec: psx-spx `OutdatedPadInitAndStart(type,
  button_dest, …)`. Settle it against the spec and make both agree.
  Ref: undocumented. Impact: low-med. Checked: yes.
- [ ] **BIOS5. `firstfile` on a non-card path returns 0.**
  `psx-bios/src/bios.rs:435`. A `cdrom:` search reads as "not found"
  instead of failing loudly or searching the CD directory.
  Ref: undocumented. Impact: low. Checked: yes.
- [ ] **BIOS6. `open` on `cdrom:` hands out descriptor 16 when all are
  taken.** `psx-bios/src/bios.rs:362`. A second overflow replaces the
  first file. The card path fails with `E_NO_FDS`. Ref: undocumented.
  Impact: low. Checked: yes.
- [ ] **BIOS7. `implemented()` counts calls that always error.**
  `psx-bios/src/bios.rs:104`. `longjmp`, `Exec`, `ReturnFromException`
  and `SystemError` count as implemented in the build's coverage check
  (`psxw-build/src/game.rs:1091`). Ref: undocumented. Impact: low.
  Checked: yes.
- [ ] **BIOS8. `printf` zero-pads negatives wrongly and ignores flags.**
  `psx-bios/src/bios.rs:597-607`. `%05d` of -5 prints `000-5`; `+`,
  space and `#` are ignored. Ref: undocumented. Impact: low (log output).
  Checked: yes.
- [ ] **BIOS9. Stale doc on `card_queue`.** `psx-bios/src/card.rs:162`
  says it returns false for an empty slot; it always returns true.
  Ref: undocumented. Impact: none. Checked: yes.

### Shortcuts outside the BIOS that reach into it

- [ ] **BIOS10. libapi's pad adapters write the BIOS's pad state.**
  `sdk-psyq/src/libapi.rs:95-109`. `InitPAD`, `StartPAD` and `PAD_init`
  set `m.kernel.pad_bufs`/`pad_started` instead of calling B0:12h, 13h
  and 15h. Ref: undocumented. Impact: low. Checked: yes.
- [ ] **BIOS11. Five libapi pad functions are no-ops.**
  `sdk-psyq/src/libapi.rs:27-31`. `EnablePAD`, `DisablePAD`,
  `SetInitPadFlag`, `ReadInitPadFlag`, `_patch_pad`. `DisablePAD` doesn't
  stop the kernel filling the buffers. Ref: undocumented. Impact: low-med.
  Checked: yes.
- [ ] **BIOS12. libcard's wrappers never call the BIOS.**
  `sdk-psyq/src/libmc.rs:60-66`. `InitCARD`, `StartCARD` and `_bu_init`
  load the cards; `StopCARD` returns 0. B0:4Ah–4Ch and A0:55h are never
  reached. Ref: undocumented. Impact: low. Checked: yes.
- [ ] **BIOS13. libpad reads the BIOS's chain list.**
  `sdk-psyq/src/libpad.rs:183` checks `m.kernel.int_handlers` for its own
  element instead of its own state. Ref: undocumented. Impact: low.
  Checked: yes.

### Shortcuts inside the BIOS

- [ ] **BIOS14. The kernel's interrupt handlers aren't chain elements.**
  `psx-bios/src/machine.rs:385-414`. Root counter events and pad buffer
  filling run in Rust when the device event fires, not as priority-ordered
  elements when the interrupt is taken. Ref: undocumented. Impact: med.
  Checked: yes.
- [ ] **BIOS15. The vblank counter event ignores I_MASK.**
  `psx-bios/src/machine.rs:392` delivers RCntCNT3 at every vblank;
  counters 0–2 are gated on I_MASK (`machine.rs:410`), and libapi's
  `StartRCnt(RCntCNT3)` sets bit 0 expecting the same gate.
  Ref: undocumented. Impact: low-med. Checked: yes.
- [ ] **BIOS16. Interrupt-acknowledge settings are ignored.**
  `psx-bios/src/bios.rs:470, 493`. `ChangeClearPad` and `ChangeClearRCnt`
  store flags nothing reads; `SetIrqAutoAck` is missing.
  Ref: undocumented. Impact: med. Checked: yes.
- [ ] **BIOS17. The pad driver doesn't use SIO0.**
  `psx-bios/src/kernel.rs:110-126` writes host pad state into the buffers
  and reports only IDs 41h and 73h. Ref: undocumented. Impact: low-med.
  Checked: yes.
- [ ] **BIOS18. The memory card driver doesn't use SIO0.**
  `psx-bios/src/card.rs`. One operation per vblank; the pre-access check
  is synchronous. Ref: D43 (OpenBIOS's events). Impact: low. Checked: yes.
- [ ] **BIOS19. `SysEnqIntRP` chains live in Rust.**
  `psx-bios/src/io.rs:739-767`. Elements linked in RAM run after them; a
  game walking the chain in RAM doesn't see the kernel's.
  Ref: D65 (the RAM half). Impact: low. Checked: yes.
- [ ] **BIOS20. The event table lives in Rust, not kernel RAM.**
  `psx-bios/src/kernel.rs:37`. Ref: undocumented. Impact: low.
  Checked: yes.
- [ ] **BIOS21. Kernel RAM is empty apart from the thread and chain
  tables.** `psx-bios/src/bios.rs:468-469` return 874h and 674h for
  `GetB0Table`/`GetC0Table`, but nothing is laid out there; the vectors,
  tables and kernel variables at 0–FFFFh read 0 (`psx-runtime/src/boot.rs`
  writes only the EXE, BSS and the thread tables). Lay the tables out with
  stand-in addresses that reach the Rust BIOS. Ref: D57 (thread tables
  only). Impact: low-med. Checked: yes.
- [ ] **BIOS22. CD file reads skip the CD controller.**
  `psx-bios/src/bios.rs:386-410`. Sectors come from the host instantly;
  no device table sits behind `open`. Ref: undocumented. Impact: low.
  Checked: yes.

### Coverage

- [ ] **BIOS0. Complete the call tables.** About 40 A0h, 43 B0h and 5 C0h
  functions and syscalls 0–3 are implemented (`psx-bios/src/bios.rs`).
  Missing, among others: `malloc`/`free`/`calloc`/`realloc` over
  `InitHeap` (whose arguments are stored and unused), the rest of the C
  library (`atoi`, `strtol`, `qsort`, …), the A0h CD and GPU functions,
  `LoadExec`, the B0h timer functions, `UnDeliverEvent`, kernel memory
  allocation, `AddDrv`/`DelDrv` and the `tty` device, syscall 4 and up
  (an exception event). Every psx-spx function either behaves as the spec
  says or fails with the reason it can't (see [Forced](#forced)). Fails
  loudly today. Ref: the `bios.rs` header ("only the calls guest code
  makes"). Impact: high for a new game.

## Emulated devices

### CD controller (`psx-devices/src/cdc.rs`, `cd.rs`)

- [ ] **DEV1. CD-DA plays nothing.** `cdc.rs:349-353, 158-169, 427-430`;
  `cd.rs:208-214`. Play (03h) stops reading and acks; stat never sets
  PLAY; Forward/Backward get INT5; mode bits CDDA, AutoPause and Report
  are never read, so there are no report INT1s and no DataEnd INT4.
  Ref: D58; open-items M13 "CD-DA". Impact: high. Checked: audit.
- [ ] **DEV2. Every command's first response comes after a flat 1 ms.**
  `cd.rs:259-262`, used at `cdc.rs:365-426`. Second responses are fixed
  multiples (MotorOn/Stop ×4, Init ×2, ReadTOC ×8, GetID ×½). Spec: per
  command; spin-up and ReadTOC take far longer. Ref: D48 and open-items
  M10 task 4 (times); the flat ack is undocumented. Impact: med.
  Checked: audit.
- [ ] **DEV3. `GetlocP` is wrong around minute boundaries.**
  `cdc.rs:389-397`. The 2-second pregap is subtracted from the BCD
  seconds with no borrow (track time 00:58 reads 01:00); the index is
  always 1; it reports the head, which is the next sector. Spec: the
  current sector's subchannel Q. Ref: undocumented. Impact: low-med.
  Checked: yes.
- [ ] **DEV4. ADPMUTE and the sound-map and request bits are ignored.**
  `cdc.rs:276-281, 248-256`. Only "apply volume" is handled; bit 0 of
  index 3 port 3 should mute XA. Ref: undocumented. Impact: low-med.
  Checked: audit.
- [ ] **DEV5. Bad and unreadable sectors are invisible.**
  `cdc.rs:461-467`. An EDC or header mismatch is only counted host-side and
  the sector is delivered; an unreadable LBA gives no interrupt and the
  read keeps scheduling. Spec: error or retry, INT5. Ref: undocumented.
  Impact: low. Checked: audit.
- [ ] **DEV6. `GetTD` past the last track returns the lead-out.**
  `cdc.rs:400-405`. Spec: INT5, error 10h. Ref: undocumented. Impact: low.
  Checked: audit.
- [ ] **DEV7. `Test` (19h) supports only subfunction 20h.**
  `cdc.rs:412-419`. Others answer [stat] with INT3. Ref: undocumented.
  Impact: low. Checked: audit.
- [ ] **DEV8. `MotorOn` always succeeds.** `cdc.rs:363-366`. Spec: INT5 if
  the motor is already on. Ref: undocumented. Impact: low. Checked: audit.
- [ ] **DEV9. No parameter validation.** `cdc.rs:340, 344-430`. Missing
  parameters read as 0; no INT5 20h or 10h; `SetSession` always
  succeeds. Ref: undocumented. Impact: low. Checked: audit.
- [ ] **DEV10. `GetID` always reports a licensed mode-2 data disc.**
  `cdc.rs:443-446`. No no-disc, audio or unlicensed case; stat fixed.
  Ref: undocumented. Impact: low. Checked: audit.
- [ ] **DEV11. stat never reports shell open, ID error or seek error.**
  `cdc.rs:158-169`. Ref: undocumented. Impact: low. Checked: audit.
- [ ] **DEV12. Status bit 2 (ADPBUSY) is always 0.** `cdc.rs:211`.
  Spec: set while XA plays. Ref: undocumented. Impact: low. Checked: audit.
- [ ] **DEV13. A partial interrupt acknowledge is ignored.** `cdc.rs:264`.
  Ref: undocumented. Impact: low. Checked: audit.
- [ ] **DEV14. A command written before the previous one's first response
  cancels that response.** `cdc.rs:298`. Hardware behaviour unverified.
  Ref: undocumented. Impact: low. Checked: audit.
- [ ] **DEV15. XA routing checks only Audio+Form2.** `cdc.rs:488`. Not the
  Realtime bit or mode 2 (Beetle reportedly requires submode & 64h).
  Ref: undocumented. Impact: low. Checked: audit.
- [ ] **DEV16. One sector buffer.** `cdc.rs:473-480`. An undelivered
  sector is replaced by the next. Ref: D66. Impact: low. Checked: audit.
- [ ] **DEV17. Reading past the data FIFO's end returns 0.**
  `cdc.rs:225-231`. Ref: undocumented. Impact: low. Checked: audit.
- [ ] **DEV18. Seek time is a guessed formula.** `cd.rs:244-257`: 20 ms
  settle plus up to 300 ms of stroke. Ref: code comment; open-items M10
  task 4. Impact: med. Checked: audit.

### SIO0 (`psx-devices/src/sio.rs`)

- [ ] **DEV19. A digital pad is the only device.** `sio.rs:83-157`. No
  config mode (43h), analog/DualShock IDs, actuators or multitap.
  Ref: D60. Impact: med. Checked: audit.
- [ ] **DEV20. JOY_STAT bits 2, 3 and 11–31 never change.**
  `sio.rs:335-349`. TX-finished, parity error and the baud timer. A poll on
  bit 2 spins forever. Ref: undocumented (module follows Beetle).
  Impact: low-med. Checked: audit.
- [ ] **DEV21. The receive side holds one byte; JOY_CTRL bits 8–9 are
  ignored.** `sio.rs:263-277, 380-381`. Ref: undocumented. Impact: low.
  Checked: audit.

### GPU front end (`psx-devices/src/gpu.rs`)

- [ ] **DEV22. GPUSTAT bits 31 and 13 follow a per-vblank field.**
  `gpu.rs:516-517`, toggled at `psx-bios/src/io.rs:657-659`. Spec: in
  240-line modes bit 31 toggles per scanline and is 0 in vblank; bit 13 is
  1 when not interlaced. Ref: undocumented. Impact: med. Checked: yes.
- [ ] **DEV23. GPUSTAT bit 25 is wrong for DMA directions 1–3.**
  `gpu.rs:518-520`. Spec: 1 = FIFO not full, 2 = bit 28, 3 = bit 27.
  Ref: undocumented. Impact: low-med. Checked: yes.
- [ ] **DEV24. GP0(1Fh) never raises IRQ1.** `gpu.rs:809` sets bit 24;
  `irq::GPU` is used nowhere. Ref: undocumented. Impact: low.
  Checked: yes.
- [ ] **DEV25. Drawing takes no time.** `gpu.rs:524-532, 353`. Bits 26 and
  28 are ready between whole commands; the FIFO is unbounded.
  Ref: D42, design §6. Impact: med. Checked: audit.
- [ ] **DEV26. GP1 decodes the exact top byte.** `gpu.rs:463, 505`.
  Mirrors (11h–1Fh, 40h–FFh) and 20h are ignored; spec decodes the low 6
  bits. Ref: undocumented. Impact: low. Checked: audit.
- [ ] **DEV27. GPUREAD past the rectangle returns 0.** `gpu.rs:551-553`.
  Spec: continues along the VRAM row. Ref: code comment. Impact: low.
  Checked: audit.
- [ ] **DEV28. The display's screen position is dropped.**
  `gpu.rs:660-684`. `present_desc` uses the display ranges only for width
  and height (see SPU9 for the renderer side). Ref: undocumented.
  Impact: low. Checked: audit.

### Timers (`psx-devices/src/timers.rs`, `clock.rs`)

- [ ] **DEV29. Sync (gate) modes are ignored.** `timers.rs:5-8, 28-36`.
  Counters always free-run. Ref: module doc only. Impact: med-low.
  Checked: yes.
- [ ] **DEV30. Timer 0's dot clock runs 4–10× too fast.** `timers.rs:31`,
  `clock.rs:56-58`. It counts the 53.69 MHz video clock, undivided. Spec:
  divided by 10/8/7/5/4 by horizontal resolution. Ref: undocumented.
  Impact: low-med. Checked: yes.
- [ ] **DEV31. Mode bit 10 always reads 1; toggle mode isn't modelled.**
  `timers.rs:94`. Ref: undocumented. Impact: low. Checked: audit.
- [ ] **DEV32. Interlaced timing isn't modelled.** `clock.rs:33-39`.
  Always 263/314 lines; the standard is fixed at boot
  (`psx-bios/src/machine.rs:263`), so GP1(08h) bit 3 doesn't change it.
  Spec: 262.5/312.5 per field, set by the GPU mode. Ref: undocumented.
  Impact: low. Checked: audit.

### MDEC (`psx-devices/src/mdec.rs`)

- [ ] **DEV33. Decoding is instant.** `mdec.rs:9-10, 160-190`.
  Ref: D58, D64. Impact: med (possibly open-items "FMV differences").
  Checked: audit.
- [ ] **DEV34. Reading an empty output FIFO returns 0.**
  `mdec.rs:192-196, 427-430`. Spec: DMA1 waits. Ref: D64. Impact: low-med.
  Checked: audit.
- [ ] **DEV35. Commands 0 and 4–7 are dropped; status bits 0–15 stay
  FFFFh.** `mdec.rs:199-207, 124`. Spec: command bits reflected in status.
  Ref: undocumented. Impact: low. Checked: audit.
- [ ] **DEV36. Status bit 30 (input FIFO full) is always 0.**
  `mdec.rs:125-128`. Ref: undocumented. Impact: low. Checked: audit.

### DMA (`psx-devices/src/dma.rs`)

- [ ] **DEV37. Transfers complete instantly.** `dma.rs:3-5, 107-110`. No
  device request gating, no chopping (CHCR bit 8), DPCR priority bits
  ignored (ready channels run lowest number first). Ref: D42.
  Impact: med. Checked: audit.
- [ ] **DEV38. A DICR write never raises IRQ3.** `dma.rs:79-86`. Spec:
  IRQ3 on any 0→1 change of bit 31, e.g. from force bit 15.
  Ref: undocumented. Impact: low. Checked: audit.

### XA (`psx-devices/src/xa.rs`)

- [ ] **DEV39. The emphasis bit is parsed but unused.** `xa.rs:61-63`.
  Whether the PS1 applies it is unverified. Ref: undocumented.
  Impact: low. Checked: audit.

### Memory card (`psx-devices/src/memcard.rs`)

- [ ] **DEV40. 'S' (Get ID) is refused.** `memcard.rs:395`. Newer Sony
  cards answer it. Ref: D65. Impact: low. Checked: audit.
- [ ] **DEV41. FLAG reports only bit 3.** `memcard.rs:333`. The error bit
  is never set. Ref: undocumented. Impact: low. Checked: audit.
- [ ] **DEV42. Writes commit instantly.** `memcard.rs:384-393`. No
  write-busy time between frames. Ref: undocumented. Impact: low.
  Checked: audit.

## SPU, GTE, renderer

- [ ] **SPU1. Muting or disabling the SPU silences CD audio.**
  `psx-spu/src/lib.rs:722-724`. The whole output is zeroed, CD/XA
  included. Spec (psx-spx SPUCNT bits 14–15): "Don't care for CD Audio".
  Ref: undocumented. Impact: med. Checked: yes.
- [ ] **SPU2. Manual-transfer FIFO drops writes past 32.**
  `psx-spu/src/lib.rs:345-361`. It reaches SPU RAM only on an SPUCNT write
  with mode 1; later DATA writes wait for the next one. Ref: undocumented.
  Impact: low. Checked: yes.
- [ ] **SPU3. DMA4 ignores SPUCNT's transfer mode.**
  `psx-spu/src/lib.rs:400-413`, `psx-bios/src/io.rs:495-513`. Spec: DMA4
  proceeds only in DMA-read/-write mode. Ref: undocumented. Impact: low.
  Checked: audit.
- [ ] **SPU4. The transfer type in RAMCNT (1DACh) is ignored.**
  `psx-spu/src/lib.rs:383-397`. Spec: values other than 2 give fill or
  repeat patterns. Ref: undocumented. Impact: low. Checked: audit.
- [ ] **SPU5. SPUSTAT bit 10 (transfer busy) is never set; bits 0–5
  mirror SPUCNT at once.** `psx-spu/src/lib.rs:290-315`. Spec: applied a
  bit delayed. Ref: undocumented. Impact: low. Checked: audit.
- [ ] **SPU6. SPUSTAT bit 11 only while RAMCNT = 0004h.**
  `psx-spu/src/lib.rs:311`. Fitted from two observed Beetle states.
  Ref: M06 Findings. Impact: low. Checked: audit.
- [ ] **SPU7. The texture cache isn't modelled.**
  `psx-render-gl/src/shaders.rs:133-148`, `lib.rs:461-495`. Texels always
  come from current VRAM. Ref: M04 Known gaps. Impact: low-med.
  Checked: audit.
- [ ] **SPU8. 15-bit colour is shifted, not expanded.**
  `psx-render-gl/src/shaders.rs:341-344`. 31 becomes 248, so white is
  F8F8F8. Ref: partly M04. Impact: low (presentation). Checked: audit.
- [ ] **SPU9. Presentation ignores interlace, PAL and the display
  position.** `psx-render-gl/src/shaders.rs:317-347`, `lib.rs:636-639`;
  `psxw-abi/src/render.rs:200-222` has no position field (see DEV28).
  Ref: undocumented. Impact: low (presentation). Checked: audit.
- [ ] **SPU10. A failed output texture or framebuffer is swallowed.**
  `psx-render-gl/src/lib.rs:620-621`. The previous frame stays; other GL
  errors in the crate fail loudly. Ref: undocumented. Impact: low.
  Checked: audit.

## SDK adapters

### libpad

- [ ] **SDK1. Digital pad only.** `sdk-psyq/src/libpad.rs:56-63, 193-194,
  281-314`. Host state is cut to 2 bytes; only 01h and 42h are answered;
  config mode, info tables and actuators never answer. Ref: D60;
  `docs/adaptor-layer-status.md`. Impact: high. Checked: audit.
- [ ] **SDK2. Port records and buffers live in the runtime.**
  `sdk-psyq/src/libpad.rs:48-52, 146-161`. Unadapted code reading
  libpad's records in guest RAM sees stale values, and the build's chip
  decision (`psxw-build/src/game.rs:975-1037`) follows calls into code
  that touches hardware or kernel RAM, not reads of SDK globals.
  Ref: design §5 table; the module doc (`libpad.rs:48-50`): "only
  libpad's own code reads them" (`psxw sdk scan`, an SDK-level check).
  Impact: med. Checked: yes.
- [ ] **SDK3. `PadSetAct` returns 0 and sends nothing.**
  `sdk-psyq/src/libpad.rs:650-656`. Spec: returns the port record and sends
  the actuator levels. Ref: module doc. Impact: low. Checked: audit.

### libetc

- [ ] **SDK4. `Vcount`, `Hcount`, the handler, callback and DMA tables
  live in the runtime.** `sdk-psyq/src/libetc.rs:4-7, 125-158, 303,
  528-530`. Code reading them in guest RAM sees zeros; the build doesn't
  detect it (same check as SDK2). Ref: design §5 table; the module doc
  (`libetc.rs:4-7`): "no code but libetc's own reads them in either game
  (`psxw sdk scan`; the games' symbol files)", partly a per-game argument;
  open-items "Other difference classes ramdiff shows". Impact: med.
  Checked: yes.
- [ ] **SDK5. `VSync`'s wait doesn't spend or restart the guest's
  budget.** `sdk-psyq/src/libetc.rs:556-563, 572-585`. Ref: D66.
  Impact: low-med. Checked: audit.
- [ ] **SDK6. `ResetCallback`, `StopCallback`, `RestartCallback` and
  `InterruptCallback(0/3)` return stand-in addresses.**
  `sdk-psyq/src/libetc.rs:69-75, 417, 436, 450`. Spec: `&intrEnv` and the
  real handlers. Ref: module doc. Impact: low. Checked: audit.
- [ ] **SDK7. Handlers and callbacks are called with a0–a3 = 0.**
  `sdk-psyq/src/libetc.rs:215-217`. Spec: I_STAT and DICR's address are
  left there. Ref: module doc. Impact: low. Checked: audit.

### libgpu

- [ ] **SDK8. `DrawSync(0)`, full-queue and `MoveImage2` waits don't
  spend the guest's budget.** `sdk-psyq/src/libgpu.rs:362-385, 395-401`.
  Ref: D66. Impact: low-med. Checked: audit.
- [ ] **SDK9. The GPU timeout counts the wrong vblanks.**
  `sdk-psyq/src/libgpu.rs:324-328`. Uses libetc's adapter count, else
  every vblank, where the original calls `VSync(-1)`. Ref: undocumented.
  Impact: low. Checked: yes.
- [ ] **SDK10. Debug checks are skipped and messages go to the host log.**
  `sdk-psyq/src/libgpu.rs:45-49, 343, 869` (no `SetGraphDebug` checks or
  `GPU_printf`); likewise `libetc.rs:250, 578` and `libcd.rs:427, 638`.
  The originals print through the game's `printf`, as libpress and libspu
  already do. Ref: module docs (partly). Impact: low. Checked: audit.
- [ ] **SDK11. `DMACallback(2, …)` returns a stand-in pump address.**
  `sdk-psyq/src/libgpu.rs:73-75`. Ref: module doc. Impact: low.
  Checked: audit.

### libmcx

- [ ] **SDK12. PocketStation commands always fail, and longer transfers
  time out.** `sdk-psyq/src/libmc.rs:35-38, 280-289`. Every command
  answers McxErrInvalid; a card's /ACK raises IRQ7 without walking the
  chains, so bytes wait for the next interrupt taken; `McxExecFlag` on a
  standard card times out. Ref: D65. Impact: low-med. Checked: audit.
- [ ] **SDK13. The last byte's wait is ~20 cycles, not a byte's time.**
  `sdk-psyq/src/libmc.rs:498-505`. `spin(m, 1)` spends one pass; a byte
  at baud 88h takes about 1,000 cycles. The comment predates D66.
  Probably a regression. Ref: undocumented. Impact: low. Checked: yes.
- [ ] **SDK14. libmcx's wire keeps its own SIO0 state and reads the
  BIOS's card images.** `sdk-psyq/src/libmc.rs:158-173, 269`. Ref: D65.
  Impact: low. Checked: audit.
- [ ] **SDK15. `_mcxStartFastDsr`'s fast path is never taken.**
  `sdk-psyq/src/libmc.rs:47-50`. Ref: D65. Impact: low. Checked: audit.

### libpress

- [ ] **SDK16. DMA1's busy flag is adapter state; CHCR1 is never busy.**
  `sdk-psyq/src/libpress.rs:33-34, 240-244, 406`. Ref: D64. Impact: low.
  Checked: audit.
- [ ] **SDK17. `DecDCTinSync`, `DecDCToutSync` and `DecDCTin`'s waits
  don't spend the guest's budget.** `sdk-psyq/src/libpress.rs:264-278`.
  Ref: undocumented (D66 names only `VSync` and libgpu). Impact: low.
  Checked: audit.
- [ ] **SDK18. `DecDCTPutEnv` and transfers skip DMA bookkeeping.**
  `sdk-psyq/src/libpress.rs:42-46`. MADR/BCR not updated after a
  transfer, DPCR turning DMA1 off not modelled, the table copy's budget
  skipped. Ref: module doc. Impact: low. Checked: audit.

### libcd

- [ ] **SDK19. `CD_datasync(0)` never waits.**
  `sdk-psyq/src/libcd.rs:659-665`. Returns 0 even with DMA3 busy.
  Ref: doc comment. Impact: low. Checked: yes.
- [ ] **SDK20. `CD_init` skips `CD_debug` output and the memory
  controller's delay registers.** `sdk-psyq/src/libcd.rs:46-48`.
  Ref: module doc. Impact: low. Checked: audit.

### libapi and libspu

- [ ] **SDK21. `SetRCnt`, `GetRCnt` and `ResetRCnt` call the timer model,
  not its registers.** `sdk-psyq/src/libapi.rs:61-63, 70, 89-90`.
  Behaviour looks equivalent. Ref: undocumented. Impact: low.
  Checked: audit.
- [ ] **SDK22. `_spu_t(3)` writes the DMA controller's fields directly.**
  `sdk-psyq/src/libspu.rs:660-663`. The original writes CHCR4 with the
  start bit. Ref: module doc (loosely). Impact: low. Checked: audit.

## Translator and runtime

### psxw-lift

- [ ] **TR1. Lifted I/O accesses are plain memory to clang.**
  `psxw-lift/src/psxw_rt.h:23-33`, `emit.rs:416-437`. A load is volatile
  only when its result is unused, and I/O hooks are added after clang
  (`psxw-link/src/rewrite.rs:195-230`). Clang may drop the first of two
  stores to one register (CD parameters), merge two reads of one FIFO, or
  reorder accesses. Spec: every access happens, in order.
  Ref: undocumented (design §1 covers widening only). Impact: med.
  Checked: yes.
- [ ] **TR2. Registers passed outside o32 arrive as 0.**
  `psxw-lift/src/emit.rs:128-146`; `analysis.rs:16-17, 109-129, 326-327`;
  `lib.rs:291-297, 363-382`. Inlining evidence is block-local; s0–s7, fp
  and HI/LO are never evidence; C callers never inline. Should fail the
  lift instead. Ref: D33; open-items "Lifted SDK asm that reads
  temporaries on entry" (C callers only). Impact: med. Checked: audit.
- [ ] **TR3. A return address other than the caller's isn't detected.**
  `psxw-lift/src/graph.rs:345`, `emit.rs:138, 279-282, 323-324`.
  `la ra,L; j f`, an edited saved-ra slot, and a game's own
  setjmp/longjmp are silently wrong. Fail the lift when r31 can differ at
  a return or tail jump (the wasm return itself is forced).
  Ref: design §4 (the r31 = 0 idiom only). Impact: low-med.
  Checked: audit.
- [ ] **TR4. A tail transfer out of an inlined callee exits the whole
  caller.** `psxw-lift/src/graph.rs:231-237, 260-264`;
  `emit.rs:198-201, 255-258, 306-314`. Spec: the target returns into the
  caller. Ref: undocumented. Impact: low-med. Checked: audit.
- [ ] **TR5. Misaligned loads and stores never fault.**
  `psxw-lift/src/psxw_rt.h:4-6, 19-33`; `psxw-cc/src/prelude.h:16-23`.
  Spec: AdEL/AdES. Ref: header comment. Impact: low. Checked: audit.
- [ ] **TR6. `bltzal`/`bgezal` with rs = ra test the new link value.**
  `psxw-lift/src/emit.rs:213-234`. r31 is written before the condition is
  read. (rs = ra is restricted on MIPS; GCC doesn't emit it.)
  Ref: undocumented. Impact: low. Checked: yes.
- [ ] **TR7. `bios_calls()` recognises only `addiu t1`.**
  `psxw-lift/src/lib.rs:524-528` (`t1_after_slot` at `lib.rs:51` also
  takes `ori`). It seeds the Asyncify scope (`psxw-build/src/game.rs:527`),
  so an `ori t1` ChangeThread trampoline is missed. Ref: undocumented.
  Impact: low. Checked: audit.

### psxw-link

- [ ] **TR8. Overlay residency is checked by two code words.**
  `psxw-link/src/game.rs:117, 838-868`; `psxw-build/src/game.rs:337-340,
  366-369, 423-429, 556-569`; `psxw-lift/src/program.rs:168-176`. The
  first candidate in name order wins; identical first words aren't
  checked; calls to addresses the main EXE also defines skip the stub;
  single-overlay addresses and intra-overlay calls aren't checked.
  Overlays sharing a slot often start with the same two instructions.
  Ref: D7, §3 (two-word compare, unchecked calls); ambiguity and
  main/overlay overlap undocumented. Impact: high. Checked: yes (the
  two-word compare).
- [ ] **TR9. Virtual time counts loop passes, not work.**
  `psxw-link/src/rewrite.rs:356-363`, `game.rs:689-708`;
  `psx-runtime/src/wasm.rs:335`, `driver.rs:144-151`;
  `psxw-cc/src/lib.rs:48`, `clang.rs:79, 95`. Straight-line code and
  recursion take no time; clang's unrolling and rotation change the count
  (no `-fno-unroll-loops`). Per-block instruction counts are feasible.
  Ref: §6, D18, D66; unrolling undocumented. Impact: high. Checked: audit.
- [ ] **TR10. Every address is masked to 24 bits.**
  `psxw-link/src/rewrite.rs:199-200, 212-213, 411-416`;
  `game.rs:780-783, 795-798`; `psxw-lift/src/analysis.rs:133-143`. RAM
  mirrors, EXP1, the BIOS ROM, KSEG2's cache control and scratchpad
  aliases fold onto RAM, zeros or the private page; a bad function pointer
  calls whatever sits at the folded index. Spec: RAM mirrored 4× in 8 MB;
  separate devices; bus errors elsewhere. Ref: design Risks (mirroring
  only). Impact: med. Checked: audit.
- [ ] **TR11. Variadic marshalling copies 8 words and doesn't realign
  64-bit arguments.** `psxw-link/src/marshal.rs:16, 96-101`.
  Ref: `marshal.rs:7-8` only. Impact: low-med. Checked: audit.
- [ ] **TR12. Writable data the address map doesn't name goes to AUX.**
  `psxw-link/src/layout.rs:147-167` (e.g. clang's `func.var` statics).
  Spec: at its original .data/.bss address. Ref: undocumented.
  Impact: low-med. Checked: audit.
- [ ] **TR13. `memory.copy`/`memory.fill` skip the I/O hook and the
  budget.** `psxw-link/src/rewrite.rs:304-328`; `game.rs:487-489`;
  `bodies.rs:89-109`. Ref: D36 (the expansion); the I/O bypass is
  undocumented. Impact: low. Checked: audit.
- [ ] **TR14. o32→C wrappers don't extend narrow parameters.**
  `psxw-link/src/marshal.rs:356, 512`. Ref: undocumented. Impact: low.
  Checked: audit.

### psxw-cc

- [ ] **TR15. `return;` in a non-void function isn't lifted.**
  `psxw-cc/src/compile.rs:141-145`, `clang.rs:31`. It stays C and returns
  undef. Ref: undocumented (D35 covers falling off the end).
  Impact: low-med. Checked: audit.
- [ ] **TR16. `-funsigned-char` is hard-wired.** `psxw-cc/src/lib.rs:81`.
  Should come from the reference flags, as soft-float does
  (`psxw-census/src/lib.rs:58`). Ref: D27. Impact: low-med (non-PsyQ
  toolchains). Checked: audit.
- [ ] **TR17. Decompiled C loops get no compiler barrier.** Lifted loops
  do (`psxw-lift/src/emit.rs:167-173`). Clang can hoist a non-volatile
  load an interrupt changes out of a polling loop. Ref: undocumented.
  Impact: low. Checked: audit.
- [ ] **TR18. Soft-float detection misses some operations.**
  `psxw-cc/src/compile.rs:381-392`. No unsigned i64 conversions,
  truncations, copysign or min/max. Ref: undocumented. Impact: low.
  Checked: audit.
- [ ] **TR19. GTE operands parse shifts tighter than `+`.**
  `psxw-cc/src/gte.rs:236-259`. The reverse of C. Ref: undocumented.
  Impact: low. Checked: yes.
- [ ] **TR20. GTE lowering drops `nop`s and ignores the load delay.**
  `psxw-cc/src/gte.rs:317-324`. Ref: undocumented. Impact: low.
  Checked: audit.

### psx-runtime and memory

- [ ] **TR21. COP0 is a plain register array.**
  `psx-runtime/src/wasm.rs:273, 349, 688-699`. SR's IEc/IM don't mask
  interrupts (only the critical section does); cache isolation is ignored;
  PRId reads 0; Cause, EPC and BadVaddr never change; `rfe` is a no-op.
  Ref: undocumented. Impact: med. Checked: audit.
- [ ] **TR22. `break` is counted and execution continues.**
  `psx-runtime/src/wasm.rs:586-589`; `psxw-link/src/bodies.rs:38-56,
  66-70`; `psxw-lift/src/emit.rs:441`. Spec: a Breakpoint exception.
  Ref: D36, §4 (continuing is undocumented). Impact: low. Checked: audit.
- [ ] **TR23. The EXE header's stack beats `SYSTEM.CNF`'s `STACK=`.**
  `psx-runtime/src/boot.rs:68`. Spec (from memory, unverified):
  `LoadExec` writes `STACK` into the header, so it wins.
  Ref: undocumented. Impact: low. Checked: yes (the code).
- [ ] **TR24. Devices and the BIOS fold guest pointers with the same
  mask.** `psx-devices/src/mem.rs:4, 63-72`;
  `psx-runtime/src/wasm.rs:196-221`. Same aliasing as TR10 (DMA itself
  wraps correctly). Ref: undocumented. Impact: low. Checked: audit.

## Forced

Not tasks: these follow from having no CPU emulation, registers in wasm
locals, or static translation. A BIOS function that can't be done fails
with the reason.

- Exceptions run as queued steps at safepoints; `ReturnFromException`
  unwinds instead of restoring registers (D45).
- `ChangeThread` saves only sp and gp into the TCB: the other registers
  are wasm locals in the caller (D57).
- The BIOS's own `setjmp` (A0:13h) returns only the first time; `longjmp`
  (A0:14h) traps.
- `Exec`/`LoadExec` of an executable that wasn't translated at build time.
- The clock moves only at safepoints; SIO0 and root counter 2 estimate the
  guest's cycle (`guest_now`, D66). A device register read sees the clock
  as of the last safepoint.
- An interrupt a device raises during a register access is taken at the
  guest's next safepoint (D64).
- Device accesses and DMA cost the CPU nothing: no wait states, no stall.
- `jr ra` is a wasm return and r31 starts at 0 (design §4).
- A `void` C function answers v0 = 0, an `int` one v1 = 0
  (`psxw-link/src/marshal.rs:189-197`; open-items Bugs).
- Uninitialised locals read 0 and clang's stack layout differs from GCC's
  (`-ftrivial-auto-var-init=zero`, D27).

## Removed from the audit

Listed so they aren't re-added:
- "A DPCR write leaves a second ready channel pending": wrong;
  `run_dma` (`psx-bios/src/io.rs:353-359`) runs every ready channel.
- XA reserved shifts 13–15 and Table5 taps 24/26: the references disagree
  (open-items M10).
- The `Instant` CD timing mode: an opt-in option (D19).
- The XA resampler's fixed starting phase: a determinism choice.
- Reading SPU DATA returns 0: the register is write-only, and this
  matches the oracle (M06).
- GTE undefined opcodes: undefined on hardware, matching Beetle
  (open-items).

## Progress log

- 2026-10-30: list created from the audit of `psx-bios`, `psx-devices`,
  `psx-spu`, `psx-gte`, `psx-render-gl`, `sdk-psyq`, `psxw-lift`,
  `psxw-link`, `psxw-cc` and `psx-runtime`.
