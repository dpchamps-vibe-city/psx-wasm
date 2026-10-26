# psx-wasm

A toolchain that turns a matching PlayStation decompilation, plus a
decompiled PsyQ SDK, into a WebAssembly build that runs natively in a
browser or webview. It is not an emulator: the game's C is compiled to wasm,
the PSX memory map is preserved, and Rust services replace the hardware at
the SDK boundary.

Status: M0 (the census) works. The design is in
[docs/design.md](docs/design.md).

```sh
cargo build --release -p psxw-cli
./target/release/psxw toolchain fetch      # pinned clang (wasi-sdk), sha256-verified
./target/release/psxw inputs sync          # snapshot pinned repos into .inputs/
./target/release/psxw census btn           # M0: compile every TU, check vs original
```

| Path | What |
|---|---|
| `inputs.lock` | Pinned commits of the decomp repos, and the pinned clang |
| `boundary/` | Per-SDK-version HLE boundary manifests |
| `games/` | Per-game manifests: the only place game-specific facts live |
| `crates/psxw-abi` | Memory layout and the guest/runtime contract |
| `crates/psxw-inputs` | Lockfile, snapshots, splat configs, symbol files, address map, originals |
| `crates/psxw-cc` | clang → wasm objects (C tier): prelude, GCC 2.95 normalization, layout probe |
| `crates/psxw-census` | M0 census: coverage, references, data and layout checks, report |
| `crates/psxw-lift` | MIPS → wasm, ahead of time (lift tier); not started |
| `crates/psxw-link` | Final link of `guest.wasm`; the relocatable wasm object reader so far |
| `crates/psx-gte` | GTE math library; not started |
| `crates/psx-runtime` | SDK services: kernel, clock, gpu, spu, cd, pad, memcard; not started |
| `crates/psxw-host-native` | wasmtime host (tests, replays, headless); not started |
| `crates/psxw-host-web` | Browser runtime (`runtime.wasm`); not started |
| `crates/psxw-cli` | `psxw` driver |
| `web/` | Worker, AudioWorklet and UI glue; not started |

The layout probe's end-to-end test runs the real reference compiler when
both variables are set:

```sh
PSXW_TEST_CC1=<path to cc1> PSXW_TEST_CLANG=.toolchain/wasi-sdk-34.0-x86_64-linux/bin/clang \
  cargo test -p psxw-cc
```
