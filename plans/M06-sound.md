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
4. [ ] libspu adapters and conformance (register decoder).
5. [ ] libsnd tick path through root-counter events. The libsnd functions
   btn uses must run as translated or C guest code with no traps.
6. [ ] SPU recording hook; record btn vectors over `title-to-gameplay`;
   capture; zero failures.
7. [ ] XA: `ReadS`, filters, decoding, resampling, CD input, real-rate
   delivery.
8. [ ] Test-disc tooling in `psxw-psx-sdk` (mkpsxiso, psxavenc); a
   generator for XA test discs; the runner's `CD_XA` kind; goldens; zero
   failures.
9. [ ] Host audio: native WAV and optional `cpal`; the web ring buffer
   (M5's worklet).
10. [ ] Remove the sound-driver exclusions from `game.toml`;
    `psxw oracle ramdiff btn --replay title-to-gameplay` reaches zero
    differences.
11. [ ] Audio determinism: identical audio hashes across two runs, and
    across hosts once M5 is done. Add everything to `psxw ci`; docs,
    README status.

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

## Findings
