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
      Native | Clean Polygons | custom
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
`set_resolution_scale(1 | 2 | 4)` API selects Native at 1× or Clean Polygons
with nearest sampling at 2×/4×. For separate style and texture options:

```rust,ignore
use psx_render_gl::visual::{ColourPrecision, TextureFilter, VisualPreset};
renderer.set_visual_preset(VisualPreset::CleanPolygons {
    scale: 4,
    textures: TextureFilter::Smooth3D,
    dithering: false,
    precision: ColourPrecision::High,
})?;
```

Clean Polygons also supports 1× independently of the Native renderer. Its
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

Clean Polygons samples native textures/CLUTs and owns its own enhanced destination
and COPY planes. Nearest sampling preserves the original upscaler's appearance.
Smooth 3D bilinearly filters decoded 4/8/15-bit polygon texture colours before
modulation and blending. A nearest sample retains transparency and mask/blend
classification; incompatible neighbours are excluded and weights renormalized.
Samples are clamped to each triangle's UV bounds as a conservative atlas guard,
not full asset identification. Axis-aligned screen/UV quads additionally use
pixel-centred sampling and the native covered texel range, avoiding seams at
sprite-atlas gutters and half-pixel shifts in cutouts. This handles flipped and
rotated UV axes without changing polygon coverage or native/wrapping fallbacks. Rectangles, lines, unusual wrapping polygons and
24-bit video retain their previous behaviour. UI drawn with polygons is filtered
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

## Tests

```sh
LIBGL_ALWAYS_SOFTWARE=1 cargo test -p psx-render-gl --features headless
./target/release/psxw web-test gpu
```

`tests/visual.rs` installs an external-style custom renderer with independent
output, exercises command/feedback delivery and lifecycle/factory failures, and
checks exact readbacks despite different GL pipeline state. `tests/upscale.rs`
checks the built-in scales, subpixel coverage/interpolation, transfers, masks,
and filtering across texture depths, palettes, windows and transparency without
changing sprites or canonical VRAM. Colour tests cover independent dithering and
precision controls, GPU seeding, non-RGB5 gradients, and both colour-plane formats'
blending, masks, uploads and copies. Tiled sprite tests cover transparent/coloured
atlas gutters, pixel-centred cutouts, mirrored/rotated UVs and every scale/filter/
precision combination. The browser suite checks golden vectors,
upscaling, both texture modes and context recovery.
