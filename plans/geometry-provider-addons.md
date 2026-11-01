# Optional geometry-provider addons

**Status:** planned; no provider protocol, discovery API, manifest syntax or
ray-based renderer is implemented.
**Goal:** let a game-specific, optional addon supply meaningful 3D rendering
input through a versioned interface, without putting game knowledge in the host
or renderer or making geometry support a prerequisite for running a game.
**First delivery:** one provider, one game, one bounded rendering path, and one
small visual consumer. Not a complete scene extractor or production ray tracer.

Read [conventions](../docs/conventions.md), the current
[host implementation guide](../docs/host-implementation.md),
[renderer extension guide](../crates/psx-render-gl/README.md), and design §7,
§Host scripting, D49 and **D56** in [design.md](../docs/design.md) before work.

## Direction and boundaries

The native-port direction is to obtain geometry where software still knows
what it is drawing. Do not export hardware-register activity and ask the host
to infer a scene. Understanding the relevant game rendering code is acceptable;
a full decompilation of the game is not required if the necessary software
boundary and data layouts are understood and validated.

```text
Game/package declaration (currently prototyped by game.toml)
                    │ selects/packages an optional provider
                    ▼
Game-side geometry addon
  knows this game's model formats, transforms and submission paths
                    │ versioned geometry feed + availability information
==================== WASM / HOST BOUNDARY =====================
                    ▼
Browser or native host
  discovers provider, checks compatibility, snapshots/transports data
                    ▼
Visual consumer / selected visual renderer
  understands the geometry protocol, not the game
                    │
                    ▼
                 visual output

Existing GPU commands → canonical VRAM execution → game-visible readbacks
                         remains authoritative
```

“Geometry feed” is intentional: a producer may submit explicit native geometry
batches, not merely attach missing values to existing PS1 GPU records. Do not
require GTE-to-GPU reconstruction if the software producer can provide the
geometry directly. Presentation timing, texture visibility and composition with
legacy rendering still need defined rules.

Responsibilities:

| Component | Owns | Must not own |
|---|---|---|
| Game/package declaration | Provider selection and packaging | An assertion that a declared provider actually works |
| Provider addon | Game-specific hooks, model decoding and conversion to the supported geometry profile | Renderer shaders, browser APIs or host-specific graphics handles |
| Provider/host protocol | Data meaning, versioning, discovery and buffer lifetime | A particular game's structures or raw GTE register traces |
| Host | Compatibility checks, lifecycle and transport | Game-specific model interpretation |
| Visual consumer | Rendering or inspecting supported geometry | Knowledge of game addresses, structures or hook names |
| Existing canonical renderer | Authoritative VRAM execution and readbacks | Depending on optional geometry to remain correct |

Game-specific facts and addon sources belong in the game package or its addon
repository, not shared toolchain/runtime/renderer crates. Shared Rust code must
remain game-agnostic. Per-game addon coverage is not proof that another game
works with only a manifest change.

## Existing support and constraints

- The browser and native hosts already load compiled Wasm mods and validate
  game identity and hook compatibility. Relevant code:
  [`web/mods.js`](../web/mods.js),
  [`psxw-host-native/src/mods.rs`](../crates/psxw-host-native/src/mods.rs), and
  [`psxw-link/src/mods.rs`](../crates/psxw-link/src/mods.rs).
- **D56 deprecates compiled mods and makes new modkit capabilities script-only.**
  The browser loader rejects compiled mods in debug builds because their
  addressing assumes the default guest window. Script mods are browser-only.
  Therefore neither existing route can be assumed to provide a portable,
  debug-compatible geometry addon without further work.
- Supporting a purpose-built compiled provider requires an explicit decision
  about D56: a documented exception, a revised policy, or another compatible
  execution route. Do not evade the policy by merely renaming a compiled mod.
  Ask before making that production architecture change.
- Existing compiled mods require an appropriately hookable/moddable base.
  Do not promise that an addon can load into every existing `game.wasm`.
- Current mod identity includes game and executable/container hashes. Determine
  whether the provider also needs source/snapshot, layout, hook-signature,
  window or other build compatibility checks; do not assume asset identity
  proves all those properties.
- The host rendering ABI currently carries XY, colours and UVs, not 3D geometry.
  `VisualRenderer` supplies a WebGL2/GLES lifecycle and output interface, not a
  geometry feed or a WebGPU abstraction.

A compiled Wasm provider is an attractive candidate artifact: C or Rust code
could execute alongside the game and potentially run in both hosts. This is an
implementation direction to validate, not an existing addon facility.

## Non-goals

- A generic plugin framework, dependency graph, provider-priority system or
  multiple competing geometry providers. Start with one selected provider.
- Full-game/scene coverage, BOF4 support, hidden-world reconstruction or recovery
  of geometry absent from the available game data.
- New lighting, ray-traced shadows/reflections, path tracing, WebGPU or hardware
  ray-tracing integration.
- Replacing canonical VRAM execution, changing GTE results or inferring depth
  from ordering-table position or coincident projected XY values.
- The separate [higher-precision GTE idea](ideas.slop.md). Reading original 3D
  inputs does not by itself add precision lost earlier in the game's pipeline.

## Tasks

### Phase 1 — Prove the software boundary and choose an execution route

- [ ] Inspect the game's real rendering path before designing a wire format.
  For BTN, investigate the source-relative
  `src/modules/projected_primitives.c`, particularly
  `EmitProjectedPrimitiveEntry`. It reads indexed vertices and primitive
  attributes, projects/culls them, constructs packets and links accepted
  primitives. This is a candidate, not proven runtime coverage.
- [ ] Verify that the candidate executes in a reproducible scene. Identify the
  source vertices/topology, software transform/projection context, appearance
  data, acceptance decisions and relevant presentation boundary. Do not mistake
  dead/always-rejected packet paths for a working provider.
- [ ] Demonstrate one small geometry batch using an existing allowed modkit or
  investigation route. Keep disposable scripts/captures in the session
  scratchpad or appropriate ignored output directories; do not modify input
  snapshots or commit game-derived data.
- [ ] Establish whether a standard SDK boundary suffices or a game-specific
  bridge is needed. Record exactly what context comes from game code.
- [ ] Inspect existing mod build/load/export facilities and identify the smallest
  viable provider route. Resolve the D56 decision with the user before adding
  production compiled-provider capabilities. State whether the first supported
  build is normal, debug or both; reject unsupported builds explicitly.

**Exit/gate:** one observed software submission has trustworthy geometry and
appearance context, and there is an agreed execution/loading route. If obtaining
it requires register-trace reconstruction, stop and report the missing software
boundary rather than substituting an emulator-style mechanism.

### Phase 2 — Implement the first game-specific producer

- [ ] Produce explicit geometry for only the demonstrated path. Preserve the
  original game calls, results, acceptance decisions and game-visible state.
- [ ] Start with original vertex positions/topology, a documented coordinate
  space and transform/projection context, and the minimum appearance data the
  first consumer needs. Flat colour is sufficient for initial validation;
  introduce UV/texture bindings when exercising a real textured batch.
- [ ] Obtain matrices/projection context from understood software inputs or
  interfaces, not by publishing hardware registers as the provider contract.
- [ ] Clearly describe coverage and omissions: which mesh/pass types are
  supported, what is culled, and whether UI, billboards or other draws are
  unrepresented. Do not label a partial batch a complete scene.
- [ ] Validate source vertices and emitted geometry against the understood
  software path. Explain expected projection/quantization differences rather
  than relaxing canonical correctness checks to fit a visual result.

**Exit:** a bounded producer yields reproducible, interpretable geometry without
altering the existing game's observable execution. No universal extractor or
stable public schema is claimed yet.

### Phase 3 — Define the smallest versioned provider/host contract

- [ ] Derive the initial protocol from the working producer and consumer needs.
  Specify scalar encoding, coordinate conventions, topology and transform/
  projection meaning. Use a small batch format, not a speculative scene graph.
- [ ] Define discovery and protocol compatibility. Prefer inspecting provider
  exports/descriptors or registration through existing loading machinery over
  mandatory new imports on every base game. Decide whether any base-game ABI
  change is actually needed; version it explicitly if it is.
- [ ] Distinguish these facts: host/consumer understands the protocol; provider
  is declared; a matching provider loaded and initialized; a particular frame's
  data is ready. An available provider can produce a valid empty frame.
- [ ] Define frame/submission scope, ownership, buffer bounds and consumption
  lifetime. Snapshot borrowed data before reuse; identify which memory owns
  offsets. Reset/restart must invalidate old geometry.
- [ ] For textured data, define binding resolution and texture/palette lifetime
  against existing inputs. Do not expose GL handles or assume end-of-frame VRAM
  always represents what an earlier draw sampled. Unsupported feedback/cache
  cases need explicit coverage limits, not invented correctness.
- [ ] Define safe observation/consumption boundaries. Avoid arbitrary re-entry
  into game code from host imports and do not retain runtime borrows across
  callbacks that can re-enter it.
- [ ] Define behavior for absent, incompatible, failed, not-yet-ready and stale
  providers. Reject invalid payloads and report actionable reasons. An ordinary
  run must remain possible without geometry support.

**Exit:** the first provider and a consumer can agree on data and availability;
a game without a provider does not require dummy geometry functions. Merely
calling an added Wasm import optional is not a compatibility solution.

### Phase 4 — Package and connect the optional capability

An illustrative declaration, **not currently supported syntax**:

```toml
[addons.geometry]
provider = "addons/btn-geometry.wasm"
```

- [ ] Add the narrow declaration/build/bundle support needed for this provider
  to the current game-package prototype. Finalize the key and artifact/source
  declaration only after the loading experiment; do not standardize a general
  addons subsystem first.
- [ ] Preserve ordinary builds and games with no declaration. Treat a manifest
  entry as a request to load, not proof of runtime availability.
- [ ] Enforce provider/game/build compatibility before installing hooks or
  interpreting game memory. Prefer existing identity checks where sufficient;
  add only the missing facts demonstrated by the producer.
- [ ] Wire discovery and data delivery in the browser and native hosts. Reuse
  existing loading facilities where they fit the agreed policy. If using the
  native render server, explicitly extend its transport; it currently has no
  geometry-feed or visual-style selection messages.
- [ ] Expose the availability result to consumers and diagnostics. If an
  experimental renderer needs unavailable data, retain the ordinary rendering
  path and explain why the mode cannot run. Do not silently render guessed or
  incomplete geometry as a complete replacement view.
- [ ] Handle reset/stop and provider failure. Initially, reject runtime provider
  replacement while active if necessary; hot-swapping is not required.

**Exit:** both hosts can determine actual availability, accept the same supported
provider data, and continue an ordinary run when the capability is unavailable.

### Phase 5 — Demonstrate one independent visual consumer

- [ ] Choose **one** small consumer: a geometry inspection view or a basic
  camera-ray renderer for the supported batch. Do not require both to justify
  the interface. Use the existing WebGL2/GLES visual extension point.
- [ ] Keep game-specific knowledge in the provider. Give the consumer only the
  validated geometry feed and explicitly supported rendering inputs. Prefer a
  narrow host-side handoff, such as a factory-supplied frame source, over new
  mandatory callbacks on every `VisualRenderer` implementation.
- [ ] Clearly label a partial-coverage demonstrator as an inspection/experimental
  view. A full game-replacement mode requires a defined composition strategy
  for remaining world geometry, overlays, sprites, video and VRAM effects; it
  is not an automatic consequence of receiving triangles.
- [ ] Leave all game-visible reads on the canonical renderer. Renderer selection
  must not change the program's canonical VRAM or computation.

**Exit:** the same provider batch can be inspected/rendered without the consumer
knowing which game produced it. No complete ray-traced game is claimed.

## Validation and completion

Keep tests small and mostly integration; extend existing harnesses rather than
creating an addon conformance framework:

- One focused provider/discovery integration case covering absence, successful
  loading, incompatible build/protocol and reset/empty-frame handling.
- One reproducible game scene checking the supported geometry and comparing
  relevant RAM, canonical VRAM, renderer-command and audio hashes with the
  provider disabled/enabled. Observation must not change virtual timing or
  game-visible state; add targeted checks only where those hashes do not cover
  an observed risk.
- One small consumer-output check and a browser/native smoke run over the same
  bounded capture. Reuse existing canonical GPU checks when rendering or its
  transport changes; do not add a scale/filter/game matrix for this feature.

Game-derived captures remain ignored; committed tests should use synthetic
geometry or reproducible capture instructions/hashes, following data-hygiene
rules. Report unsupported cases as unsupported, not passing coverage.

Before marking this done, update the host guide, renderer/modkit documentation,
manifest documentation and relevant design decisions to describe what actually
shipped. Record the D56 resolution and any ABI change. The default game build
must remain usable without the provider, and the first provider's game/build
and rendering coverage must be documented explicitly.

## Open decisions (resolve at the relevant gate)

- Provider execution route and its relationship to D56; normal/debug coverage.
- Precise software boundary and transform context for the first BTN provider.
- Discovery location, payload handoff and whether the base-game ABI needs any
  change, versus a separately versioned addon/host protocol.
- Initial appearance/texture profile and supported legacy-composition scope.
- Manifest source/artifact syntax and the required build-identity checks.

## Progress log

- Initial plan: direction agreed; implementation has not started. First action
  is the software-boundary/loading feasibility gate, not an ABI expansion.

## Findings

- Compiled mod loading already exists in both hosts, but D56 and the browser's
  debug-window refusal make reuse conditional, not a solved dependency.
- BTN's `EmitProjectedPrimitiveEntry` is a source-level candidate where model
  vertices and primitive construction meet. Runtime coverage and suitable
  software transform/projection inputs remain unproven.
- Existing canonical GPU commands, recorder hooks and `VisualRenderer` do not
  provide a 3D feed. No equivalent BOF4 software boundary has been established.
