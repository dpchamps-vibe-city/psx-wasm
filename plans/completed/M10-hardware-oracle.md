# M10 — Hardware oracle

**Status:** closed (tasks 5 and 6 done). Its open items are in
[`open-items.md`](../open-items.md).
**Depends on:** M3, and a rig. Can run any time after M3.
**Design refs:** design: Oracles (the upgrade path); decisions D22–D24.

## Goal

Replace the stand-in with the truth: run the whole vector archive on a
real PlayStation, using the same runner program. Store hardware goldens
alongside the emulator's, promote verdicts from provisional to verified,
and turn every emulator–hardware disagreement into a documented finding.

This also gives CD drive timing its first real oracle, and the BIOS its
real one.

## Context from M3

- The mailbox protocol (`crates/psxw-oracle/src/wire.rs`, generated C
  header) is transport-independent: batches of ops in, a result record
  per op out. A serial transport frames the same batches.
- On the emulator the runner boots from a disc; on hardware it is
  uploaded. `oracle/runner/start.s` sets its own stack, turns interrupts
  off and needs no BIOS state, except for BIOS_CALL.
- GP0 is paced per packet (wait for GPUSTAT bit 26, then send whole
  commands); never per word on bit 28.
- Hardware goldens go in target directories named `hw-…`;
  `psxw oracle check` already prefers them to emulator goldens.

## The rig

- **Console:** an original PS1 model with the serial port (the later PSone
  doesn't have one).
- **Cable:** a USB-serial adapter wired to the PS1 serial port at the
  correct voltage levels, or a ready-made PS1 serial cable.
- **Running unsigned code:** FreePSXBoot (a memory card exploit, which
  needs a memory card and a way to write it) or Unirom (by cheat cartridge
  or softmod). Both are open source.
- **Alternative:** a community member with a rig runs exported vector
  batches and returns the goldens. The provenance records the contributor
  rig id.

## Scope

**In**
- **A serial transport** for the runner: the same mailbox protocol,
  framed over the serial link, at the fastest reliable baud. The runner
  build gains a serial build variant.
- **`psxw oracle capture --target hardware --port <tty>`:** upload the
  runner (Unirom/nops-compatible upload, or a FreePSXBoot payload), then
  stream vectors and collect results.
- **Batch export and import** for community capture, with signed
  provenance metadata (a rig id, and a console model and BIOS version
  reported by the runner).
- **CD timing envelope:**
  - `CD_SEQ` vectors on a test disc (built with M6's tooling): seeks
    across distances and reads at both speeds, repeated N times;
  - the distribution of latencies, recorded per vector;
  - psx-wasm's realistic timing model must fall inside it, which makes CD
    timing verified;
  - the model is tuned only if it falls outside the envelope.
- **BIOS_CALL vectors** against the real BIOS.
- **Re-verification:**
  - rerun every archived vector;
  - recompute verdicts: hardware golden means verified;
  - diff emulator goldens against hardware goldens, and file each
    disagreement as a finding (and upstream to the emulator project where
    appropriate).

## Tasks

1. [ ] The runner's serial transport and serial build variant; the host
   side of the transport in `psxw-oracle`.
2. [ ] Upload paths (Unirom/nops and FreePSXBoot payload); document rig
   setup in `oracle/hardware.md`.
3. [ ] Capture the archive (resumable, with progress).
4. [ ] CD timing envelope vectors and measurement; tune the model if
   needed; mark CD timing verified.
5. [x] Verdict recomputation; emulator–hardware disagreement report.
   *(Recomputation was already in `check`, which judges against
   hardware goldens first. `psxw oracle disagree` writes the report.)*
6. [x] Batch export and import for community capture. *(`psxw oracle
   export`/`import`, `psxw_oracle::batch`. Provenance carries hashes,
   not signatures.)*
7. [ ] Add hardware-golden verification to `psxw ci` (when goldens
   exist); docs, README status.

## Exit criteria (all automated, once a rig exists)

1. Every archived vector has a hardware golden.
2. `psxw oracle check <game> --device all` against hardware goldens gives
   zero failed. Every previously provisional verdict is now verified, or a
   failure has been fixed in psx-wasm.
3. psx-wasm's realistic CD timing falls within the measured envelope for
   every `CD_SEQ` vector.
4. The emulator–hardware disagreement report is published in Findings.

## Questions for hardware

- **XA reserved shifts 13-15.** The emulator shifts by that much;
  psx-spx says they act as 9. psx-wasm follows the emulator for now
  (`psx_devices::xa::header`). Real encoders never write these values.
- **Table5 of the XA zigzag interpolation.** The emulator, and the
  tables' symmetry, give taps 24 and 26 as -2 and 0; psx-spx prints -1
  and +1.

## Progress log

## Findings

- **GTE questions only hardware can settle (from M4).** See
  [`M04-pixels.md`](M04-pixels.md), Findings.
  - The set `disputed-gte`: GPL's MAC overflow flag at exactly 2^43−1.
    The emulator flags it; psx-spx and psx-gte don't.
  - The N/A opcodes 00h and 1Ah, which the emulator runs as RTPS and DCPL.
  - Behaviours GTE_EXEC cannot reach: SXYP and IRGB writes, and FLAG
    writes.
