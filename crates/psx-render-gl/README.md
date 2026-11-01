# Host renderer

`Renderer` owns the exact 1× VRAM engine. It decodes the existing renderer ABI,
executes native draws/transfers/cache updates, and alone answers game readbacks.
Visual output is delegated to a `visual::VisualRenderer`:

```text
renderer command stream
          │
   exact VRAM engine ───────────────→ game readbacks
          │ decoded commands + canonical texture/CLUT inputs
          ▼
   selected VisualRenderer ────────→ RGBA output → canvas/frame dump
      Native | PSXRevamp | custom
```

For the game/host ABI, host startup and browser/native wiring, see
[Implementing a host](../../docs/host-implementation.md). The visual-renderer
interface below is internal to the host, separate from that ABI.

## Add a visual renderer

Implement `visual::VisualRenderer` in a module or another Rust crate, then install
it through a host-local factory:

```rust,ignore
renderer.set_visual_renderer(|ctx| {
    Ok(Box::new(MyRenderer::new(ctx)?))
})?;
```

No adaptor, runtime, packet decoder, guest ABI, or exact rasterizer changes are
needed. A new implementation is not required to use the built-ins' packed setup
records, shaders, output dimensions, or internal-resolution model. Add a host UI
option/factory selection if users should be able to select it. The existing
`set_resolution_scale(1 | 2 | 4)` API selects Native at 1× or PSXRevamp
with nearest sampling and Original sprite reconstruction at 2×/4×. For separate
style, texture and sprite options:

```rust,ignore
use psx_render_gl::visual::{ColourPrecision, SpriteReconstruction, TextureFilter, VisualPreset};
renderer.set_visual_preset(VisualPreset::PsxRevamp {
    scale: 4,
    textures: TextureFilter::Smooth3D,
    sprites: SpriteReconstruction::Xbr,
    dithering: false,
    precision: ColourPrecision::High,
})?;
```

PSXRevamp also supports 1× independently of the Native renderer. Its
configuration is host-local; custom factories still define their own options.
The browser's `web/rendering.js` registers built-in names and option controls;
`WebRenderer.set_visual_options` selects their factories.

The interface:

- **`VisualContext`** supplies the current GL context, effective drawing state,
  and canonical draw/read VRAM and CLUT textures. These inputs are read-only by
  contract: never render into, overwrite, or delete canonical resources.
- **`execute`** receives decoded ABI commands in order, after exact execution.
  DRAW contains original ABI primitive records, not a renderer-specific layout.
  Feedback primitives are expanded into clipped row draws, interleaved with
  exact row synchronization; use `ctx.state` rather than caching SET_STATE.
- **`flush`** finishes buffered work before canonical inputs change and before
  an execute call returns. Uploads may be buffered between these boundaries.
- **`present`** returns a `VisualFrame`: an implementation-owned framebuffer
  with RGBA8 at COLOR_ATTACHMENT0, dimensions, and GL bottom-first rows.
  `Renderer` handles frame readout and screen blitting without knowing the
  visual renderer's representation.
- **`reset`** discards visual history when native VRAM resets.
- **`take_stats`** optionally reports additional GL work.
- **`destroy`** releases visual resources while the GL context is current,
  before canonical resources disappear. The factory must clean up partially
  allocated resources on failure.

Factory replacement is transactional: failure retains the previous renderer and
output. Successful replacement invalidates the previous frame, but never resets
native VRAM. Factories can seed a new R16UI colour plane via `ctx.vram.seed`.
After context loss, the host recreates the exact engine, restores its native
shadow, then reinstalls the selected visual renderer/factory. Visual history
is not part of guest state.

Callbacks share the GL context, not an implicit pipeline-state contract. Set the
state your implementation needs; the exact engine reestablishes its integer draw,
blit and pixel-transfer state after callbacks. This is a **WebGL2/GLES extension
point**, not a dynamic plugin loader or cross-graphics-API interface.

## Built-in implementation details

- `lib.rs`: command orchestration, exact VRAM, CPU readbacks, output consumption.
- `visual.rs`: public extension contract, built-in factory, native display.
- `enhanced.rs`: upscaled setup, shaders, colour planes, hazards and presentation.
- `raster.rs`: optional shared GL record-drawing and upload-packing helpers.
- `presentation.rs`: optional shared 15/24-bit-to-RGBA presentation helper.
- `setup.rs` / `shaders.rs`: exact PS1 setup and reusable pixel arithmetic.

PSXRevamp samples native textures/CLUTs and owns its own enhanced destination
and COPY planes. Nearest sampling preserves the original upscaler's appearance.
Smooth 3D bilinearly filters decoded 4/8/15-bit polygon texture colours before
modulation and blending. A nearest sample retains transparency and mask/blend
classification; incompatible neighbours are excluded and weights renormalized.
Samples are clamped to each triangle's UV bounds as a conservative atlas guard,
not full asset identification. Axis-aligned screen/UV quads additionally use
pixel-centred sampling and the native covered texel range, avoiding seams at
sprite-atlas gutters and half-pixel shifts in cutouts. This handles flipped and
rotated UV axes without changing polygon coverage or native/wrapping fallbacks.
With Original sprite reconstruction, rectangles, lines, unusual wrapping polygons
and 24-bit video retain their previous behaviour. UI drawn with polygons is filtered
as well: the command stream does not identify whether a polygon is world or UI.
Dithering is an independent option: Original keeps the native 4×4 offsets on
primitives requesting dithering, while Off clears them only in visual setup.
Native precision uses R16UI RGB5/mask planes. High precision uses R32UI packed
RGB8 plus a separate mask bit, retaining eight bits/channel through shading,
texture filtering, blending and COPY. Native texels/uploads expand 31 to 248,
without inventing texture detail. R32UI initialization converts canonical words
on the GPU; it cannot use a raw R16UI blit. Packed 24-bit video still reads native
VRAM directly. Original dithering in High mode keeps the offsets but quantizes
to eight bits instead of five; High + Off is the cleanest combination. This is
colour precision, not higher-precision GTE coordinates. Other visual renderers
may use different representations without altering exact execution.

Sprite reconstruction is independent of polygon texture sampling. Original keeps
all existing sampling behaviour. xBR replaces sampling on magnified textured
rectangles and axis-aligned screen/UV quads with a transparency-aware adaptation
of Hyllian's MIT-licensed xBR-lv2, corner C with small-detail preservation
(`sprite_xbr.glsl`). Its decoded, bounded 21-tap neighbourhood reconstructs both
colour and fractional coverage; coverage composites over the current visual
background after PSX semitransparency, with mask checks retained. STP differences
restrict colour mixing only when semitransparency is enabled. UV half-texel offsets
stay in texture space, and taps rather than the interpolation phase are clamped.
Unmagnified and unusual wrapping primitives retain native sampling/fallbacks.
This selects geometry, not characters: UI and tiled artwork can qualify too.
No enhanced texture cache or canonical VRAM writes are introduced.

MMPX is an independent `SpriteReconstruction::Mmpx` option on the same geometry.
It applies McGuire and Gagiu's MIT-licensed, palette/feature-preserving 2× rules
(`sprite_mmpx.glsl`) to decoded colours and binary transparency. The virtual 2×
result is sampled directly and nearest-scaled to the output; 4× rendering does
not recursively apply MMPX. It selects original texels rather than interpolating
new colours or fractional alpha, retaining the selected STP bit for blending and
mask checks. Unmagnified sprites retain native sampling. Polygon texture sampling
and canonical VRAM execution are unchanged.

Bicubic (`SpriteReconstruction::Bicubic`, `sprite_bicubic.glsl`) uses a separable
4×4 Catmull–Rom kernel rather than pixel-pattern rules. Decoded RGB and binary
coverage are filtered in premultiplied form; local central-2×2 colour/coverage
bounds suppress ringing without introducing transparent black fringes. STP class
selection, modulation, dithering, mask checks and visual-background compositing
remain separate. It uses the same sprite geometry/atlas bounds and native
unmagnified fallback as the other options, with no change to surface sampling.

Super-xBR 3p (`SpriteReconstruction::SuperXbr3p`) implements the full linked
3p-smoother preset: three offscreen stages followed by Jinc2-sharper resampling
in primitive shading. Native textures/CLUTs are decoded on the GPU into independent
premultiplied RGBA8 intermediates; semitransparent primitives use separate STP
planes. A persistent 16 MiB/256-entry tile cache invalidates on intersecting native
SYNC, reset and clipped-row feedback callbacks, rather than rebuilding every draw.
Paletted tiles use snapshot identities independent of rotating CLUT slot numbers;
unchanged reloads reuse images without suppressing canonical CLUT loads. Actual
palette updates and partial 16/256-entry loads retain their visibility semantics.
Ordinary polygons and adjacent same-tile sprites retain
batching. Constant anti-ringing ranges skip unnecessary Jinc work without changing
its result. The additional processing remains potentially expensive; correctness
tests do not establish a target frame time. See [`super_xbr/README.md`](src/super_xbr/README.md)
for the pipeline, memory and mixed MIT/GPL-2.0-or-later source notices.

## Tests

```sh
LIBGL_ALWAYS_SOFTWARE=1 cargo test -p psx-render-gl --features headless
./target/release/psxw web-test gpu
```

`tests/visual.rs` installs an external-style custom renderer with independent
output, exercises command/feedback delivery and lifecycle/factory failures, and
checks exact readbacks despite different GL pipeline state. `tests/sprites.rs`
checks transparent silhouette coverage, palettes, atlas bounds, flipped/rotated
UVs, opaque/STP/mask behaviour, independent surface sampling and canonical VRAM.
A synthetic upstream-rendered xBR reference fixture checks tiny-feature
interpolation on a quad with unequal screen/UV scales. A separate MMPX reference
fixture checks palette colours, tiny features and transparency on rectangles and
quads at each internal scale and colour precision. Bicubic colour and coverage
are also checked against an independent double-precision Catmull–Rom calculation.
Super-xBR is compared with a reference generated from all four upstream stages;
the tests also check warm-cache draw counts, batching, source updates, rotating
CLUT slots, existing palette state and partial-load cache visibility.
`tests/upscale.rs`
checks the built-in scales, subpixel coverage/interpolation, transfers, masks,
and filtering across texture depths, palettes, windows and transparency without
changing sprites or canonical VRAM. Colour tests cover independent dithering and
precision controls, GPU seeding, non-RGB5 gradients, and both colour-plane formats'
blending, masks, uploads and copies. Tiled sprite tests cover transparent/coloured
atlas gutters, pixel-centred cutouts, mirrored/rotated UVs and every scale/filter/
precision combination. The browser suite checks golden vectors,
upscaling, both texture modes and context recovery.
