# Conventions

Rules every milestone follows. When a milestone needs something these don't
cover, add it here, so the next milestone inherits it.

## Generality

- **Nothing in Rust code is specific to one game or one ROM.** Game facts
  (paths, include dirs, defines, symbols such as the frame counter, replay
  files, per-function tier pins) live in `games/<id>/game.toml`. SDK facts
  live in `boundary/`.
- **Prefer conventions all splat-based decomps share** (symbol_addrs files,
  auto-names `<prefix>_<ADDR>`, `target_path` and `sha1`, the
  `INCLUDE_ASM` → `.include "…/NAME.s"` expansion) over one repo's own
  files. Isolate anything harness-specific behind an adapter, and document
  it as such.
- **Normalizer rules and translator patterns target compiler or dialect
  idioms** (GCC 2.95, GNU C, PsyQ), never one file's quirk.

## Inputs, pins and local configuration

- `inputs.lock` pins:
  - input repos by commit;
  - toolchains by URL and sha256 (clang via wasi-sdk, binaryen);
  - Docker images by source commit, and by image digest once built;
  - the disc track hashes for each game.
- Input repos are read through `git archive` snapshots in
  `.inputs/<name>-<sha7>/`. The local checkouts are only object stores:
  never read their working trees, and never write to them.
- Machine-local paths (disc images, any override) go in `psxw.local.toml`
  (gitignored). Never search the filesystem for inputs: if a path is
  missing, fail with a message saying which key to set.
- `psxw toolchain fetch` downloads and verifies every pinned toolchain.
  `psxw inputs sync` snapshots every pinned repo.

## Data hygiene (legal)

Never commit anything derived from a game disc, a BIOS dump, the PsyQ SDK
or the decomp's binaries. That covers:
- disc images and extracted files;
- translated (generated) C and compiled game objects, `game.wasm`;
- vectors, goldens, frame and RAM dumps;
- translated-SDK caches;
- the runtime archive *built with* game data.

They live in gitignored directories: `.inputs/`, `.toolchain/`, `.oracle/`,
`out/`. Hashes of such data may be committed. The precompiled runtime
archive and the Docker images contain only our code and open-source
components.

## Commands

- **Every capability is a `psxw` subcommand.** Every oracle is a command
  that exits non-zero on failure and writes a JSON report plus a
  human-readable summary under `out/`.
  - Naming: `psxw <verb> <game> [options]`, e.g. `psxw census btn`,
    `psxw difftest btn`, `psxw run btn --replay R --frames N`,
    `psxw oracle check btn --device gpu`.
  - Aggregate gate: `psxw ci <game>` runs every oracle of every milestone
    marked done, and is what "from a clean checkout" means in exit
    criteria.
- **Reports separate verdict classes:** verified, provisional, unresolved,
  failed. Only `failed` (and, where a milestone says so, `unresolved`) fails
  a command.

## Determinism

- Game-visible behaviour depends only on (disc, build, replay, runtime
  options). Wall-clock time only paces presentation.
- No iteration over `HashMap`/`HashSet` where order affects output; use
  `BTreeMap`, or sort.
- Random test inputs come from seeded generators. The seed is in the
  report.
- Every run can emit RAM and VRAM hashes per frame. Determinism checks
  compare two runs.

## File formats (all versioned; versions bumped on change)

| Format | Where defined | Notes |
|---|---|---|
| Host interface and renderer commands | `psxw-abi` | ABI version constant; the linker refuses mismatches |
| Replay | `psxw-abi::replay` | Header (game id, disc sha1, build id, runtime options) plus per-frame pad states |
| Vectors | `psxw-oracle::vector` | Content-addressed (sha256 of the canonical encoding); type tag plus payload; large state (VRAM, SPU RAM) stored once and referenced by hash |
| Goldens | `psxw-oracle::golden` | Result bytes plus a provenance JSON (`target`, `commit`, `bios_sha1`, `runner_version`, `created`) |
| Reports | each crate | JSON, `schema_version` field |

## Rust

- Edition 2024, toolchain pinned by `rust-toolchain.toml`.
- Libraries use `anyhow::Result` at boundaries and typed errors where a
  caller branches on them. No `unwrap` on input-derived data.
- Match the surrounding code: doc comments on public items, terse comments
  only where intent isn't obvious.
- Unit tests next to the code. Tests needing pinned external tools (clang,
  cc1, Docker) skip with a message when the tool isn't configured, and run
  in `psxw ci`.
- Runtime crates that go into `game.wasm` are `no_std` + `alloc` where
  practical, and must build for `wasm32-unknown-unknown`.

## Commits

- Commit at coherent points with descriptive messages: what changed and
  why.
- **Never** add `Co-Authored-By` or any Claude or Claude Code attribution
  to commits or PRs.
- Don't commit generated or game-derived data (see Data hygiene).

## Documentation

- `docs/design.md` changes whenever an architectural decision changes.
  Record the change in `docs/design.md` (its decision log) too.
- Each milestone file keeps its Status, ticked Tasks, Progress log and
  Findings current. Findings that affect later milestones are also copied
  into those milestones' Context sections.
- The README's milestone index status is updated when a milestone changes
  state.
