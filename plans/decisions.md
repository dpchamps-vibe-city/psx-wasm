# Decision log

Settled decisions, with their reasons. Each points to where
`docs/design.md` elaborates. To change one, record the proposal and
evidence in the current milestone's Findings and ask the human. Don't
silently diverge.

## Product and scope

**D1. What psx-wasm is.** A toolchain that turns a matching PlayStation
decompilation into a native WebAssembly build. It does no CPU emulation
and ships no register-level hardware model. It is a source port plus a
compatibility layer: high-level reproduction of the console's OS and
device behaviour, in the same family as libultraship, PsyCross and Wine.
*Why:* the human wants a truly native result, not an emulator.
(design: Goal; What "native, not an emulator" means)

**D2. psx-wasm vs a static recompiler.** Static recompilers translate all
machine code and emulate the hardware around it. psx-wasm compiles the
source and translates only the gaps, so as decomp coverage rises it
becomes a source port. Its value is a moddable, enhanceable port and a
general toolchain, not merely playability.

**D3. Game-agnostic.** Harvest Moon: Back to Nature (btn) is the first
target, not the design centre. Game facts live only in
`games/<id>/game.toml`, SDK facts in `boundary/`.

**D4. Not built.**
- a MIPS interpreter or dynamic recompiler;
- register-level GPU, CD or pad emulation;
- a BIOS ROM.

Open: register-level SPU as a fallback for unknown libspu versions (M8).

## Memory and calls

**D5. Keep the PSX address space.** Every symbol sits at its original
address in a 16 MiB window at `0x0100_0000`. Effective addresses are
`(addr & 0x00FF_FFFF) + base`, and pointers keep their PSX values. *Why:*
no source patches, pointers in assets keep working, data is verifiable,
and RAM is comparable by symbol. The M0 layout probe validated it (8,374
of 8,374 entries match). (design §1)

**D6. Function pointers are MIPS addresses.** One dispatch table,
`table_index = (addr & 0x003F_FFFC) >> 2`. Indirect calls use the o32
indirect ABI `(a0..a3) -> i64`. (design §2)

**D7. Overlays dispatch on resident bytes.** A stub compares code words in
RAM against each candidate overlay. *Why:* it's how hardware decides, and
needs no per-game loader hook. (design §3)

## Implementations

**D8. One implementation per function.** In priority order:
1. an *adapter*, if the function touches hardware registers or BIOS
   internals;
2. else *decompiled C*;
3. else *translated*.

The rule is applied per function or member, never per library.
(design §4, §5)

**D9. Translate into generated C.** It is compiled by the same clang
pipeline, not emitted as wasm, Rust or LLVM IR. *Why:* one pipeline, clang
optimizes it, it's debuggable through `#line`, and N64Recomp and
PSXRecomp prove the approach. (design §4)

**D10. The translator reads original images.** Per game, it reads the
original EXE and overlay images, which is valid because of D5. Relocatable
SDK members are used only for the reusable translated-SDK cache. *Why:* no
dependency on building the decomp repo, and no export bundle needed.
(design §4)

**D11. Generated functions use the o32 form `u64 f(a0..a3)`.** The linker
puts adapters at every edge where C and generated code call each other,
derived from the C side's wasm signature plus prototype facts (structs
passed by value, variadics). *Why:* no dependence on prototypes for asm
functions, and functions can change tier freely. (design §4)

**D12. The SDK is split into three runtime layers:**
- stable, hardware-defined **devices**;
- the **BIOS**, identical for every SDK version;
- thin **adapters**, keyed by object identity (hash), with struct layouts
  generated from each SDK version's headers.

(design §5)

**D13. libapi is translated.** 33 of the 39 members btn links are BIOS
trampolines, which the translator routes to the Rust BIOS. Adapters are
needed only for `counter.o` (it programs the timers) and for the members
that patch the Sony BIOS (`pad.o`, `patch.o`, `chclrpad.o`). (design §5)

**D14. Pure libraries run as guest code.** libsnd, libgs, libgte, libmath
and libc are never replaced for correctness; an adapter is allowed only
for speed, where equivalence is easy to show. *Why:* their behaviour is
what the game shows, plays and decides (`rand`, the music sequencer).
(design §5)

**D15. The decompiled SDK is not on the critical path.** Translation
covers everything. Decomp effort pays off most on the boundary libraries,
where it specifies the adapters.

## Runtime and host

**D16. The runtime is a precompiled Rust archive linked into
`game.wasm`.** Swapping it means a relink. Dev mode keeps it a separate
module sharing memory. The host interface is the only seam, and tests run
the exact wasm that ships. (design §7)

**D17. psx-wasm owns the final link.** wasm-ld can't express placement.
`wasm-ld --emit-relocs` plus a re-homing pass is the fallback. (design §8)

**D18. Time is virtual and deterministic.** It moves at blocking calls and
safepoints, and callbacks run at safepoints. The browser runs it in a Web
Worker paced with `Atomics.wait`; JSPI is the fallback. (design §6)

**D19. CD timing is a runtime toggle** (realistic or instant), with a
per-game default. (design §6)

**D20. The GPU uses WebGL2, not WebGPU.** A front end runs in the runtime;
the backend runs in the host, in Rust over `glow`, which gives WebGL2 in
the browser and GLES natively.
- VRAM is an `R16UI` texture.
- All pixel maths is integer shader code: rasterization, interpolation,
  blending, mask and dither.
- The destination is read from a read copy of VRAM, and batches split
  where primitives overlap.
- The goal is byte-exact output at 1×. Upscaling and other enhancements
  layer on top later, as unverified options.

This supersedes "software rasterizer first" and "RGBA8 + stencil".
(design: Runtime)

## Verification

**D21. The oracle rule.** Every component needs an automated oracle we
didn't write. A component without one is *provisional* (shippable,
labelled unverified), never "correct". (design: Oracles)

**D22. Oracle kinds:**
- the original code (decompiled C, original libraries);
- results exact by construction (checksums, hashes, fully determined
  results);
- device targets: hardware is a true oracle, an emulator a provisional
  one.

Models (our own reference code, transcribed specs) are debugging aids
only.

**D23. One emulator, dockerized.** Beetle PSX (Mednafen's PS1 core), built
from source at a pinned commit in Docker, with OpenBIOS built from source,
is the device target until hardware exists. No two-emulator consensus.
Known-weak areas (CD drive timing) stay unresolved. *Why:* the human
judged one accurate emulator sufficient, and wants it containerized.
(design: Oracles)

**D24. Hardware later, through the same runner** (M10). Every golden
records its provenance. When hardware arrives, the whole archive reruns.

## Inputs and process

**D25. Inputs are pinned** in `inputs.lock` and read through `git archive`
snapshots. No worktrees, and no builds of the decomp repos.

**D26. Original bytes come from splat** `target_path` + `sha1`, not from
harness-specific manifests.

**D27. Toolchain.**
- clang 23.1.0 from wasi-sdk 34 (freestanding wasm32);
- UB containment flags `-std=gnu89 -fwrapv -fno-strict-aliasing
  -fno-delete-null-pointer-checks -funsigned-char -ffreestanding
  -ftrivial-auto-var-init=zero`;
- Rust 1.94.1.

**D28. GCC 2.95-isms are normalized on preprocessed text**, never by
editing sources. The rules found in M0:
- incomplete-element arrays are moved after their struct, or dropped;
- register-pinning asm is stripped (file scope becomes `static`);
- any `.include "…/NAME.s"` file-scope asm becomes a lift marker;
- `break` becomes `__psxw_break`;
- empty asm barriers are kept;
- functions with real asm or GNU nested functions are lifted, inner
  nested functions included.

**D29. Host support functions.** GCC inlined some builtins (`abs`,
`labs`, `fabs`), so the original binaries carry no copy; the runtime
supplies them.

**D30. Milestone order.**
- Translator before boot.
- The oracle harness before devices (it can run in parallel with M1–M2).
- Browser before sound, though that pair is swappable.

**D31. Commits are allowed but carry no Claude attribution.** Nothing
game-derived is ever committed.

**D32. No filesystem searching.** Inputs outside psx-wasm and the named
decomp repos come from paths the human supplies.

## Open questions (tracked)

- **Q1.** Webview shell (Tauri) or browser tab only. Decided in M5.
- **Q2.** Register-level SPU fallback for unknown libspu versions.
  Decided in M8.
- **Q3.** The license for psx-wasm. Needed before any public release.
- **Q4.** The second game. Decided in M9.
