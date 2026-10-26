# M6 — Sound

**Status:** not started
**Depends on:** M2, M3. Parallel with M4–M5. **Unlocks:** M7.
**Design refs:** design: Runtime (spu, cd), §5 (libsnd as guest code),
Device behaviour notes (XA streaming, the timing coupling); decisions D14,
D19, D21–D23.

## Goal

The game sounds right, and every piece is checked against an oracle:
- **The SPU device:** voices, envelopes, interpolation, noise, pitch
  modulation, reverb, CD audio input, and the capture buffers.
- **XA audio streamed from the disc.**
- **libspu adapters.**
- **libsnd,** the sequencer, running as translated guest code, ticked by
  root-counter events.

## Context

- M2 provides the CD data path (realistic/instant timing), the scheduler,
  root counters and events, plus stub SPU adapters.
- M3 provides the `SPU_RUN` runner kind, the emulator target, and
  `ramdiff`. The `psxw-psx-sdk` image is where the test-disc tools go.
- From M3's Findings:
  - mkpsxiso is already in `psxw-psx-sdk` (bundled with PSn00bSDK v0.24),
    and the runner itself boots from a disc built with it;
  - Beetle's CD controller refuses data reads from a disc it does not
    identify as licensed (SYSTEM.CNF boot name `S[CLI]xx_…`, or licence
    text in sector 4), so test discs need the runner disc's naming;
  - SPU_RUN starts at a capture-buffer half boundary (SPUSTAT bit 11,
    which Beetle reports only while transfer control is 0004h). SPU_RESET
    leaves ENDX = 00FFFFFFh and reserves SPU RAM 1000h–100Fh. Captures
    are DMA-read right after the register snapshot while the SPU keeps
    running: provisional, refine here (and bump `RUNNER_VERSION`);
  - the Beetle option `spu_silent_voice` is forced off: on, it leaves SPU
    register state inexact.
- From M4's Findings: SPU_RUN results depend on what ran before them in
  the same emulator session. The handwritten `spu_noise_voice_3`
  (`d61a8b7b…`) reproduces its golden alone, but differs (709 bytes) in a
  full `capture` next to M4's 1,320 synthetic GPU vectors. Make SPU_RUN
  independent of session history before trusting SPU goldens.
- btn facts:
  - it reads the SPU's key status in 6 places;
  - it uses `SsSetTickMode` and `SsSeqCalledTbyT` (sequencer ticks from a
    timer event);
  - it has XA audio on its disc;
  - it links no libpress, so there's no MDEC video.
- **Key status and the "voice ended" bits are game-observable.** libsnd
  reads them to allocate voices, so envelope timing must be exact.

## Scope

**In**
- **SPU device (`psx-devices::spu`):**
  - 512 KiB of SPU RAM; transfers (DMA or I/O semantics at the adapter
    level);
  - 24 voices: ADPCM decoding (filters, shift, loop start and end, flags);
    pitch, with the 4-point Gaussian interpolation table from psx-spx;
    ADSR with exact rate tables; volumes with sweep; noise; pitch
    modulation;
  - key on/off and the ENDX bits;
  - the reverb algorithm over its SPU RAM work area;
  - CD audio input (XA and CD audio) with its volumes;
  - the capture buffers: CD L/R and voices 1 and 3 written to SPU RAM;
  - the SPU IRQ address (if any game uses it);
  - main volume and mixing at 44.1 kHz.
- **XA:**
  - `ReadS` streaming with file and channel filtering;
  - XA-ADPCM decoding (4 and 8 bits, mono and stereo, 37.8 and 18.9 kHz);
  - resampling to 44.1 kHz per psx-spx;
  - feeding the SPU's CD input;
  - real-rate sector delivery on the virtual clock;
  - CD audio track playback (`Play`) for games that use it (generic).
- **libspu adapters** (by the M1 hardware-access classification) and
  conformance against the translated original libspu, through a test-only
  SPU register decoder.
- **libsnd** runs as guest code. Tick delivery comes from the root-counter
  events libsnd sets up.
- **Host audio:**
  - native: a WAV writer for headless runs, and optional `cpal` playback;
  - web: fill the M5 AudioWorklet SharedArrayBuffer ring.

  Audio is the master clock for presentation pacing only; game-visible
  time stays virtual.
- **Oracles:**
  - an SPU recording hook: per frame, the register-level write log with
    sample timestamps, plus the SPU RAM state before it;
  - **CD and XA vectors through a generated test disc:**
    1. tools in `psxw-psx-sdk`: mkpsxiso (builds the disc image) and
       psxavenc (encodes XA), both open source;
    2. generated PCM signals are encoded to XA;
    3. a disc is built with the runner as its boot EXE plus XA and data
       files;
    4. the emulator boots the disc, and the runner issues `ReadS` with
       filters and captures the CD audio capture buffers.

**Out:** MDEC; the CD timing envelope (hardware only, M10); audio
enhancements such as better interpolation, which would be later and
unverified.

## Design details

- **SPU state is serializable,** for vectors and determinism checks.
  Mixing advances in sample steps tied to the virtual clock: one vblank
  yields the exact sample count for the video mode, with fractional
  accumulation.
- **Register-level recording.** Adapters translate libspu calls into
  device operations. The recorder logs the equivalent SPU register writes,
  which the SPU vocabulary already closely mirrors, so `SPU_RUN` vectors
  can replay them on the emulator or hardware.
- **Comparison regions:**
  - the capture buffers (CD L/R, voice 1, voice 3);
  - the reverb work area;
  - ENDX, key status and envelope registers.

  To check any voice's output, the generator routes the behaviour under
  test to voice 1 or voice 3.

## Tasks

1. [ ] SPU device: ADPCM, voices, ADSR, interpolation, noise, pitch
   modulation, mixing; unit tests from spec examples (debug aids).
2. [ ] Reverb and capture buffers; SPU IRQ address.
3. [ ] SPU synthetic vectors, each isolating one feature at a time on
   voices 1 and 3. Capture goldens; reach zero failures in
   `psxw oracle check --device spu`.
4. [x] libspu adapters and conformance (register decoder). *(D47: libspu,
   and libsnd for its direct writes, run as original code over the
   register-level SPU, so there are no adapters to decode.
   `psxw conform btn --lib libspu` plays a voice through the original
   library and must be audible.)*
5. [x] libsnd tick path through root-counter events. The libsnd functions
   btn uses must run as translated or C guest code with no traps.
   *(`psxw conform btn --lib libsnd`: `SsSeqCalledTbyT` runs exactly 60
   times over 60 vblanks under `SS_TICK60` and 240 under `SS_TICK240`. The
   sequences btn plays during gameplay run in the disc-gated runs.)*
6. [ ] SPU recording hook; record btn vectors over `title-to-gameplay`;
   capture; zero failures.
7. [ ] XA: `ReadS`, filters, decoding, resampling, CD input, real-rate
   delivery. *(All but the CD input, which waits for the SPU device:
   `psx_devices::xa`, the libcd adapters' stream. Decoding and resampling
   match the emulator exactly; see task 8.)*
8. [x] Test-disc tooling in `psxw-psx-sdk` (mkpsxiso, psxavenc); a
   generator for XA test discs; the runner's `CD_XA` kind; goldens; zero
   failures.
   *(`psxw_oracle::xa_disc` generates the sectors directly, so psxavenc
   isn't needed. 28 vectors are consistent with the emulator, and with
   them `psxw oracle check btn --device xa` reports zero failures.)*
9. [x] Host audio: native WAV and optional `cpal`; the web ring buffer
   (M5's worklet). *(`host.audio`, ABI 5: each vblank hands the host the
   frames due in virtual time, 768 cycles each. `psxw run --audio-wav`;
   the Worker fills the worklet's ring. Both hash the frames the same way,
   and `web-test game` compares them. `cpal` playback isn't added; it's
   optional.)*
10. [ ] Remove the sound-driver exclusions from `game.toml`;
    `psxw oracle ramdiff btn --replay title-to-gameplay` reaches zero
    differences.
11. [ ] Audio determinism: identical audio hashes across two runs, and
    across hosts once M5 is done. Add everything to `psxw ci`; docs,
    README status. *(Both checks exist: `psxw run --check determinism`
    adds `audio-determinism`, and `web-test game` compares the audio with
    the native host's. Both are in `psxw ci` through the `boot` and
    `web-game` gates, and their runs wait for the disc.)*

## Exit criteria (all automated)

1. `psxw oracle check btn --device spu`: every synthetic and recorded SPU
   vector matches its golden (provisional). Zero failed.
2. `psxw oracle check btn --device xa`: every XA test-disc vector matches
   its golden (provisional). Zero failed.
3. libspu adapter conformance passes on all recorded call vectors.
4. `psxw oracle ramdiff btn --replay title-to-gameplay` gives zero
   differences with no sound-driver exclusions.
5. Audio hashes are identical across two native runs (and between native
   and browser if M5 is done).
6. `psxw ci btn` includes 1–5.

## Risks and decision points

- **The emulator's XA resampler and reverb are models.** Disagreements
  with hardware surface in M10. Until then they're provisional by
  definition.
- **The test-disc boot path** depends on the emulator booting a generated
  disc with OpenBIOS. Covered by M3's BIOS gate; re-check here.
- **Real-rate XA streaming** couples virtual time to the sector rate.
  Verify that instant CD timing mode either disables XA rate-coupling
  correctly, or is documented as incompatible with streamed audio.

## Progress log

- **2026-10-26** (this commit): task 7 up to the SPU. The SPU device
  itself is being written alongside (tasks 1-3).
  - **`psx_devices::xa`:**
    - XA-ADPCM decoding as psx-spx gives it: 4- and 8-bit, mono and
      stereo, shift 13-15 acting as 9, and per-channel history across
      sectors;
    - the 25-point zigzag resampler to 44.1 kHz, with 18.9 kHz samples
      fed twice. The resampler is provisional: psx-spx calls it "nearly
      correct", and its table's DC gain is about 0.91.
  - **libcd adapters:**
    - With XA-ADPCM mode set, a Form2 audio sector goes to the decoder
      and isn't delivered as data. With the filter bit set, only sectors
      matching `Setfilter`'s file and channel play.
    - `Mute`/`Demute` gate the output.
    - The samples go to `Machine::cd_audio`, the seam the SPU's CD input
      will take them from.
    - XA streams run at the disc's real rate even under instant timing,
      which settles the plan's open risk on rate coupling.

- **2026-10-26** (this commit): task 9.
  - **ABI 5:** `host.audio(ptr, frames)` carries 44.1 kHz stereo i16.
    `Machine::audio_vblank` hands over exactly the frames that fell due:
    one per 768 cycles of virtual time, so 737.14 per NTSC frame on
    average. Their source is `cd_audio` until the SPU mixes (silence
    when empty).
  - **Native host:** `psxw run --audio-wav PATH` streams a WAV file, and
    the run report's `audio` gives frames, audible frames and an FNV-1a
    hash.
  - **Web host:** the Worker writes into the AudioWorklet's ring and
    hashes the frames the same way, in 32-bit halves (checked against a
    BigInt reference). `psxw web-test game` compares the audio frames and
    hash with the native host's.
  - **Check:** a conformance run (38 vblanks) delivers 28,015 silent
    frames.

- **2026-10-26** (this commit): task 5.
  - **The libsnd scenario** (`psxw conform btn --lib libsnd`, in the
    `conform` gate) counts entries to `SsSeqCalledTbyT` through a build
    probe (`psxw_build::game::Options::probes`, read mid-run with
    `Conform::counts`). It runs ResetCallback, SsInit, then each tick
    mode with SsStart for 60 vblanks and SsEnd.
  - **Both tick sources reach libsnd's tick:**
    - `SS_TICK60`: libsnd's own vblank callback, through original libetc
      (D45), ran 60 times;
    - `SS_TICK240`: root counter 2's events, through libapi's counter
      adapters and the BIOS event chain, ran 240 times.
  - **Guest libraries are compared with themselves:** a library whose
    boundary policy is guest is now built the same on both sides.
    Forcing libsnd into a device library would send its direct SPU
    register writes (`_SsInit`) to a bus that has no SPU yet.

- **2026-10-26** (this commit): task 8, XA through the CD controller.
  - **The test file:** `psxw_oracle::xa_disc` generates `XATEST.XA`, and
    `psxw oracle images build` writes it onto the runner disc (mounted at
    `/gen`; mkpsxiso `xa` file type) and records its LBA.
    - It has 7 regions, one per coding plus a filter-0 region, each
      interleaved at the factor that plays it in real time at double
      speed.
    - Each region has 4 played channels, with filler channels in the
      other slots.
    - Each channel's first sector uses filter 0, so decoding from it
      doesn't depend on history.
  - **The runner's `OP_CD_XA`** (`oracle/runner/cd.c`, linked last, so
    the other ops' code keeps its addresses):
    - it sets SPUCNT for CD audio and the CD's audio matrix;
    - Setmode, Setfilter, Setloc, ReadS by polling;
    - it acknowledges interrupts while 0.8 s pass, DMA-reads the CD
      capture buffers, and pauses.
    - A batch holds at most one, which keeps each exchange inside the
      host's time limit.
  - **The check** (`check --device xa`) decodes and resamples the same
    sectors. Each 512-sample capture ring must appear exactly in that
    stream, at any position and resampler phase: when the runner read
    it is the emulator's timing. The captures aren't byte-reproducible
    across sessions, because the seek starts from wherever the previous
    vector left the head. So capture keeps such goldens
    (`verdict::timing_variable`).
  - **What the oracle settled.** Against psx-spx, five changes, now in
    `psx_devices::xa` marked `oracle:`. Found by fitting the captures
    (`examples/xa_probe` dumps them):
    - **Filter arithmetic:** `(old*f0 >> 6) + (older*f1 >> 6)`, not
      `(old*f0 + older*f1 + 32) / 64`. Only that made the filtered
      regions consistent.
    - **Reserved shifts 13-15** shift by that much. psx-spx says they
      act as 9 on hardware.
    - **Resampler rounding:** the 29 products are summed, then shifted
      by 15. psx-spx truncates each division by 8000h.
    - **Table5's taps 24 and 26** are -2 and 0, not -1 and +1. Table5 is
      then exactly Table1 reversed, as the tables' symmetry implies, so
      psx-spx probably has a misprint. Least squares over the filter-0
      region found it: 44 of 4,096 samples off before, 0 after.
    - **18.9 kHz** isn't 37.8 kHz with each sample doubled. It is the
      same polyphase filter at 3/7 of an input per output, 7 outputs
      every 3 inputs, with the tables in the order of their fractions.
  - **Result:** all 28 vectors are consistent (provisional). Before the
    fixes, 24 of 28 failed.
  - **The rest of the archive is undisturbed:** after the runner change,
    a full recapture reproduced all 8,062 other vectors byte for byte.

## Findings
