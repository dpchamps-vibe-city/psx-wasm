# Optional geometry-provider addons

**Status:** planned; no provider protocol, discovery API, manifest syntax or
ray-based renderer is implemented.
**Goal:** provide a game-agnostic mechanism through which optional addons expose
versioned rendering capabilities, without putting game knowledge in the host or
renderer or making geometry support a prerequisite for running a game.
**First infrastructure milestone:** discovery, capability publication, transport
and lifecycle, demonstrated with synthetic data independently of BTN extraction.
**Separate first game milestone:** one BTN provider, one bounded rendering path,
and one small visual consumer. Not a complete scene extractor or production ray
tracer. The first producer's coverage does not define the infrastructure's limits.

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
                    │ named, versioned capability payloads + availability
==================== WASM / HOST BOUNDARY =====================
                    ▼
Browser or native host
  discovers provider, checks compatibility, snapshots/transports data
                    ▼
Visual consumer / selected visual renderer
  understands selected capability schemas, not the game
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
| Provider addon | Game-specific hooks, decoding and publication of supported capabilities | Renderer shaders, browser APIs or host-specific graphics handles |
| General provider infrastructure | Discovery, capability identification/versioning, availability, transport and buffer lifetime | A mandatory geometry representation or game-specific payload interpretation |
| Capability schemas | The meaning and encoding of particular rendering data | A particular game's structures or raw GTE register traces |
| Host | Compatibility checks, lifecycle and transport | Game-specific model interpretation |
| Visual consumer and its decoders | Understanding selected schemas, validating their payloads and rendering/inspection | Knowledge of game addresses, structures or hook names |
| Existing canonical renderer | Authoritative VRAM execution and readbacks | Depending on optional geometry to remain correct |

Game-specific facts and addon sources belong in the game package or its addon
repository, not shared toolchain/runtime/renderer crates. Shared Rust code must
remain game-agnostic. Per-game addon coverage is not proof that another game
works with only a manifest change.

## Separate infrastructure, schemas and producers

There are two contracts, and a game-specific implementation of them. Do not
collapse them into a universal `GeometryFrame` with mandatory triangles,
transforms, colours or materials.

### General provider infrastructure

This is a capability discovery and data-publication mechanism. It knows how to
attach a provider, discover the named/versioned capabilities it offers, deliver
publications, and manage availability, ownership, synchronization and failure.
It does not know how BTN stores a model, or require a model to be triangles at
all. Payloads are opaque to this layer: it checks envelope and buffer bounds;
a consumer's schema decoder checks the payload's meaning and internal bounds.

An illustrative logical exchange, **not a decided wire format or Wasm API**:

```text
Provider description:
  transport protocol version
  offered capability schema IDs and versions

Publication:
  capability schema ID and version
  reset/restart generation and submission/presentation scope
  bounded payload bytes

Consumer:
  selects capabilities and versions it understands
  checks current data availability and coverage before using them
```

Provider identity/build validation remains part of loading. A declaration, a
loaded provider, an offered capability and ready data are different facts. A
consumer may understand the transport but none of a provider's payload schemas.
Related publications need explicit snapshot association and lifetime rules so
consumers do not accidentally combine different frames or generations.

This infrastructure can be implemented and tested with synthetic publications
without understanding BTN. Such a test does not prove real game extraction or
settle the production execution route. D56 still applies to provider loading.

### Capability-specific contracts

Each capability defines a particular kind of rendering information. Examples
might include `render.indexed-meshes/v1`, `render.heightfield/v1`,
`render.instances/v1`, `render.materials/v1` or `render.lights/v1`. These are
illustrative names, not reserved syntax, mandatory features or a closed list.

A schema owns its scalar encoding, coordinate conventions, topology, transforms,
references, appearance and coverage semantics as applicable. An indexed-mesh
schema might start with triangles, transforms and colours; those requirements
belong to that schema, **not to the general provider protocol**. Do not design
all these schemas now. Validate each against actual producer/consumer needs.

Evolution rules:

- More data within an existing schema, such as more represented objects, need
  not change either protocol. Coverage still has to be stated accurately.
- A provider can offer a new capability without changing the loader or opaque
  transport. A consumer needs a decoder for that schema; flexibility does not
  mean automatically understanding arbitrary new data.
- Transport protocol compatibility and capability-schema compatibility are
  separate. Breaking changes to payload meaning require an incompatible schema
  version, not silent reinterpretation under the same identifier/version.
- Unsupported capabilities can be left unused. A consumer must not ignore data
  that a selected schema requires for correct interpretation; dependencies,
  optional additions and their defaults must be explicit in that schema.
- Renderer eligibility depends on current capabilities, data and coverage, not
  merely on whether a provider loaded. Missing requirements need a diagnostic
  and an explicit fallback, not invented geometry or material properties.

Schemas describe shared rendering concepts, not BTN structs. A renderer may
support several representations without requiring every provider to emit them
all. Keep the mechanism narrow: one selected provider, no provider marketplace,
dependency resolver or speculative scene framework.

### Game-specific producers and visual consumers

The BTN provider understands BTN's software boundaries and translates its data
into whichever capabilities it can supply. Another game's provider can expose
the same capabilities using entirely different extraction logic. Expanding BTN
coverage is producer work; adding ray intersections, acceleration structures,
lighting or shadows is rendering-backend work. Neither belongs in the transport.

Game-specific extraction is not a shared SDK adaptor. A validated SDK boundary
may provide some inputs, but BTN model/world interpretation stays in its addon,
not in game-agnostic native backends or hosts.

The first synthetic producer proves infrastructure, not a universal geometry
schema. The first BTN producer proves a bounded extraction path, not the maximum
information the mechanism can eventually expose.

## What richer rendering would require

A camera-ray renderer using the original assets need not look radically different
from a modern raster renderer using the same geometry. Perspective-correct
texturing and avoiding final screen-coordinate rounding are not unique to rays;
neither approach recovers detail or precision already absent from its inputs.

A more dramatically lit, diorama-like view requires sufficient source information
and additional renderer features, not a special ray-tracing transport. For
example, a bounded farm-area provider could describe loaded models and placement
before view culling, including relevant off-screen shadow casters. Nearby hidden
surfaces matter for ambient occlusion; reflections can need objects outside the
camera view. Accepted draw submissions alone do not establish that coverage.

Such information can be additional data under existing schemas or new optional
capabilities. Once sufficient data exists, adding an effect need not change any
provider contract. A renderer-defined sun also need not require a game-provided
light capability. Texture transparency, lighting already painted into artwork,
sprite/billboard treatment and legacy composition still require deliberate
handling. Sprites do not become volumetric objects merely by casting rays.

This is future headroom, not additional first-delivery scope. There is no promise
that BTN exposes a convenient loaded-world boundary, and no reconstruction of
geometry absent from its available data.

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

- A general-purpose plugin platform, dependency resolver, provider-priority
  system or multiple competing geometry providers. General capability discovery
  and publication are in scope; start with one selected provider.
- Full-game/scene coverage, BOF4 support, hidden-world reconstruction or recovery
  of geometry absent from the available game data.
- Implementing new lighting, ray-traced shadows/reflections, path tracing, WebGPU
  or hardware ray-tracing integration in the first delivery. This is not a
  permanent limitation on capability schemas or future consumers.
- Replacing canonical VRAM execution, changing GTE results or inferring depth
  from ordering-table position or coincident projected XY values.
- The separate [higher-precision GTE idea](ideas.slop.md). Reading original 3D
  inputs does not by itself add precision lost earlier in the game's pipeline.

## Tasks

These are separate work tracks, not a requirement to finish BTN extraction before
building infrastructure. Track A can ship with synthetic data. B1 gates B2, not
A. A production loading route still requires its policy/compatibility decision;
synthetic success must not be used to bypass it. Track C integrates the real game
provider; track D can first exercise a synthetic capability and later BTN data.

### A — General provider infrastructure and synthetic proof

- [ ] Inspect existing mod build/load/export facilities and choose the smallest
  viable attachment/execution route. Resolve D56 with the user before adding
  production compiled-provider capabilities. State normal/debug and host support
  explicitly; reject unsupported builds. An in-process publication test alone
  does not prove a portable addon loader.
- [ ] Define the transport envelope and discovery/compatibility rules separately
  from any payload schema. Prefer provider exports/descriptors or registration
  through existing loading machinery over mandatory imports on every base game.
  Decide whether a base-game ABI change is needed; version it explicitly if so.
- [ ] Discover named/versioned capabilities without a required mesh, triangle,
  material or scene representation. Specify how consumers select understood
  schemas and report unmet requirements. Do not bake the synthetic example's
  fields or schema IDs into the generic loader/transport.
- [ ] Distinguish declaration, successful initialization, offered capabilities,
  supported consumer schemas and current publication readiness. Define absence,
  incompatibility, failure, not-yet-ready and stale-data behavior. Ready empty
  data is valid where the selected schema defines it.
- [ ] Define publication scope, snapshot association, generation, ownership,
  buffer bounds and consumption lifetime. Identify which memory owns offsets;
  snapshot borrowed bytes before reuse. Reset/restart invalidates old data.
- [ ] Define safe observation/consumption boundaries. Avoid arbitrary re-entry
  into game code from host imports and runtime borrows held across callbacks
  that can re-enter it. Validate envelopes in infrastructure and payloads in
  schema-aware decoders; report actionable failures.
- [ ] Wire discovery and opaque publication delivery in browser and native hosts
  using the agreed route. If using the native render server, explicitly extend
  its transport; it currently has no capability-feed or visual-style selection
  messages. Do not give the transport game-specific interpretation duties.
- [ ] Demonstrate the mechanism with a synthetic producer and a small consumer.
  A triangle example may exercise one schema, but must not define the envelope.
  Include an unrecognized capability to check that discovery and transport do
  not require understanding every payload or treating it as geometry.
- [ ] Handle reset/stop and distinguish unavailable data from an execution
  failure. A failed load can leave ordinary rendering intact; a trap or corrupted
  shared execution state may require restarting without the provider rather than
  merely disabling its output. Hot-swapping is not required.

**Exit:** generic discovery, delivery and lifecycle work independently of BTN,
with synthetic coverage over the agreed loading route and both hosts. Games
without providers need no dummy capability functions. Calling a new Wasm import
optional is not an instantiation compatibility solution. This milestone does not
claim a working BTN provider or freeze a public rendering schema.

### B1 — Prove the BTN software boundary

- [ ] Inspect the game's real rendering path before finalizing its payload schema.
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
- [ ] Check observation's timing non-interference during the proof, not only at
  final validation. Preserving arguments/results alone does not establish that
  hooks or instrumentation leave virtual timing unchanged.

**Exit/gate for B2:** one observed software submission has trustworthy geometry
and appearance context, and the investigation has an allowed execution route.
If obtaining it requires register-trace reconstruction, stop this producer track
and report the missing software boundary rather than substituting an emulator-
style mechanism. This does not block game-agnostic infrastructure work.

### B2 — Implement the first BTN producer and its capability schema

- [ ] Produce explicit geometry for only the demonstrated path. Preserve the
  original game calls, results, acceptance decisions and game-visible state.
- [ ] Select or define the capability schema from the demonstrated producer and
  consumer needs. Specify scalar encoding, coordinate conventions, topology and
  transform/projection meaning. Use a small batch schema, not a speculative scene
  graph; keep all of these choices out of the general transport envelope.
- [ ] For this producer's first schema, start with original vertex positions/
  topology, documented transforms and the minimum appearance data the consumer
  needs. Flat colour is sufficient for initial validation. This is a bounded
  producer choice, not a required profile for every provider.
- [ ] Introduce UV/texture bindings when exercising a real textured batch. Define
  binding resolution and texture/palette lifetime against existing inputs. Do
  not expose GL handles or assume end-of-frame VRAM represents what an earlier
  draw sampled. Unsupported feedback/cache cases need explicit coverage limits,
  not invented correctness.
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

### C — Package and connect the optional BTN provider

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
- [ ] Connect the BTN provider to track A's discovery and publication route in
  both hosts. Verify the declared schema versions and actual payloads, without
  adding BTN-specific branches to shared loader, transport or rendering code.
- [ ] Expose capability availability and coverage to consumers and diagnostics.
  If an experimental renderer needs unavailable data, retain the ordinary
  rendering path and explain why the mode cannot run. Do not silently render
  guessed or incomplete geometry as a complete replacement view.
- [ ] Exercise reset/stop and track A's failure policy with the real provider.
  Initially reject runtime provider replacement while active if necessary.

**Exit:** both hosts can determine actual availability, accept the same supported
provider data, and continue an ordinary run when the capability is unavailable.

### D — Demonstrate one independent visual consumer

- [ ] Choose **one** small consumer: a geometry inspection view or a basic
  camera-ray renderer for the supported batch. Do not require both to justify
  the interface. Use the existing WebGL2/GLES visual extension point.
- [ ] Declare the consumer's supported capability schemas and requirements.
  Keep game-specific knowledge in the provider. Give the consumer only validated
  payloads and explicitly supported rendering inputs. Prefer a narrow host-side
  handoff, such as a factory-supplied publication source, over new mandatory
  callbacks on every `VisualRenderer` implementation.
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

- Infrastructure milestone: one focused synthetic provider/discovery integration
  case covering absence, successful loading, incompatible build/transport/schema,
  unknown capabilities, invalid bounds and reset/empty-data handling. Demonstrate
  that a consumer can use a supported capability without interpreting unrelated
  payloads. No BTN data or extraction is needed for this gate.
- BTN milestone: one reproducible game scene checking supported geometry and comparing
  relevant RAM, canonical VRAM, renderer-command and audio hashes with the
  provider disabled/enabled. Observation must not change virtual timing or
  game-visible state; add targeted checks only where those hashes do not cover
  an observed risk.
- One small consumer-output check and browser/native smoke run with the same
  synthetic publications for the infrastructure milestone; reuse that path for
  the bounded BTN capture at game integration. Reuse existing canonical GPU
  checks when rendering or its transport changes; do not add a scale/filter/game
  matrix for this feature.

Game-derived captures remain ignored; committed tests should use synthetic
geometry or reproducible capture instructions/hashes, following data-hygiene
rules. Report unsupported cases as unsupported, not passing coverage.

Record infrastructure and BTN-provider completion separately; synthetic success
must not mark the game provider complete, and unproven BTN extraction must not
hide a completed infrastructure milestone. For each delivered track, update the
relevant host, renderer/modkit, manifest and design documentation to describe what
actually shipped. Record the D56 resolution and any ABI change. The default game
build must remain usable without a provider; document the real provider's game/
build and rendering coverage explicitly when it ships.

## Open decisions (resolve at the relevant gate)

- Provider execution route and its relationship to D56; normal/debug coverage.
- Precise software boundary and transform context for the first BTN provider.
- Discovery location, opaque publication handoff and whether the base-game ABI
  needs any change, versus a separately versioned addon/host transport protocol.
- Capability identifier/version rules, publication association and schema-level
  requirements/defaults. No universal geometry representation is assumed.
- Initial synthetic and BTN capability schemas, appearance/texture semantics and
  supported legacy-composition scope; these do not constrain other providers.
- Manifest source/artifact syntax and the required build-identity checks.

## Progress log

- Initial plan: direction agreed; implementation not started. Initially placed
  the software-boundary/loading feasibility gate before infrastructure work.
- Discussion refinement: separate general provider infrastructure, capability
  schemas, game-specific extraction and visual consumers. Infrastructure can be
  delivered with synthetic data independently of BTN feasibility. Triangles,
  transforms and colours are possible schema contents, not mandatory transport
  fields. No implementation, loading-policy exception or wire format is approved
  by this clarification.

## Findings

- Compiled mod loading already exists in both hosts, but D56 and the browser's
  debug-window refusal make reuse conditional, not a solved dependency.
- BTN's `EmitProjectedPrimitiveEntry` is a source-level candidate where model
  vertices and primitive construction meet. Runtime coverage and suitable
  software transform/projection inputs remain unproven.
- Existing canonical GPU commands, recorder hooks and `VisualRenderer` do not
  provide a 3D feed. No equivalent BOF4 software boundary has been established.
- General infrastructure is discovery, publication and lifecycle plumbing; its
  implementation does not require solving BTN extraction. Production loading,
  memory safety and host transport still need explicit decisions and validation.
- A provider can expand through more data under existing schemas or additional
  capabilities without changing the general transport. Consumers must understand
  the selected schemas; arbitrary metadata does not automatically become useful
  rendering input.
- A bounded accepted-draw provider may be enough for inspection, but does not
  establish off-screen/hidden geometry coverage for shadows or reflections.
  Broader extraction is producer work and richer shading is renderer work, not
  a reason to bake those features into the infrastructure contract.
