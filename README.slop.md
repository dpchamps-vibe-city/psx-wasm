# psx-wasm

A toolchain that turns a matching PlayStation decompilation, plus a
decompiled PsyQ SDK, into a WebAssembly build that runs natively in a
browser or webview. It is not an emulator: the game's C is compiled to wasm,
the PSX memory map is preserved, and Rust services replace the hardware at
the SDK boundary.

Status: M0–M3 done; M4–M8 in progress. btn builds into `game.wasm` and
boots from the user's disc (`psxw.local.toml`) into its main loop,
deterministically, natively and in the browser.
Milestones and their state: [plans/README.md](plans/README.md). The design
is in [docs/design.md](docs/design.md); decisions in
[docs/design.md, "Decision log"](docs/design.md#decision-log).

```sh
cargo build --release -p psxw-cli
./target/release/psxw --help               # commands; use <command> --help for options
./target/release/psxw toolchain fetch      # pinned clang, binaryen, wasm-bindgen, signatures
./target/release/psxw inputs sync          # snapshot pinned repos into .inputs/
./target/release/psxw ci btn               # every gate of every milestone so far
./target/release/psxw build btn            # out/btn/game.wasm
./target/release/psxw run btn --check boot # needs [game.btn] disc in psxw.local.toml
./target/release/psxw build-web btn && ./target/release/psxw serve out/web/btn
```

| Path | What |
|---|---|
| `inputs.lock` | Pinned commits of the decomp repos, toolchains, oracle images |
| `boundary/` | The SDK boundary: per-library policy (adapter, device, guest), object identities |
| `games/` | Per-game manifests: the only place game-specific facts live |
| `crates/psxw-abi` | Memory layout, the guest/runtime/host contract, renderer commands, replays |
| `crates/psxw-inputs` | Lockfile, snapshots, splat configs, address map, originals, SDK identity |
| `crates/psxw-cc` | clang → wasm objects (C tier): prelude, normalization, layout probe |
| `crates/psxw-census` | M0 census; SDK struct layouts for adapters |
| `crates/psxw-lift` | MIPS → C translation (lift tier) |
| `crates/psxw-link` | Links `guest.wasm`: placement, dispatch, overlays, guest passes |
| `crates/psxw-difftest` | M1: decompiled C vs translation, function by function |
| `crates/psxw-build` | `psxw build` (game.wasm = guest + runtime via wasm-merge), `build-web` |
| `crates/psx-runtime` | The runtime module: boot, driver, FFI to guest and host |
| `crates/psx-bios` | The HLE BIOS: A0/B0/C0, events, files, memory cards, the I/O bus |
| `crates/psx-devices` | Clock, timers, CD, GPU front end, DMA, IRQ, SIO0 pad, memory card |
| `crates/psx-gte` | GTE, exact per psx-spx and the oracle |
| `crates/sdk-psyq` | PsyQ adapters, validated objects, generated struct layouts |
| `crates/psx-render-gl` | The WebGL2/GLES backend, render server |
| `crates/psxw-host-native` | wasmtime host: headless runs, replays, conformance driver |
| `crates/psxw-host-web` | The browser renderer (wasm-bindgen) |
| `crates/psxw-oracle` | Device oracle: vectors, goldens, Docker emulator, play and ramdiff |
| `crates/psxw-gpu-oracle` | The GPU as an oracle device (render server in Docker) |
| `crates/psxw-cli` | The `psxw` driver |
| `web/` | The browser host: page, Worker, OPFS disc, AudioWorklet |
| `oracle/` | Emulator host, runner, Docker images |

The layout probe's end-to-end test runs the real reference compiler when
both variables are set:

```sh
PSXW_TEST_CC1=<path to cc1> PSXW_TEST_CLANG=.toolchain/wasi-sdk-34.0-x86_64-linux/bin/clang \
  cargo test -p psxw-cc
```
