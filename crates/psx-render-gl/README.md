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
      Native | Upscaled | custom
```

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
`set_resolution_scale(1 | 2 | 4)` API selects the built-in presets.

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

The upscaler samples native textures/CLUTs and owns its own enhanced destination
and COPY planes. Other visual renderers may use different representations without
altering exact execution.

## Tests

```sh
LIBGL_ALWAYS_SOFTWARE=1 cargo test -p psx-render-gl --features headless
./target/release/psxw web-test gpu
```

`tests/visual.rs` installs an external-style custom renderer with independent
output, exercises command/feedback delivery and lifecycle/factory failures, and
checks exact readbacks despite different GL pipeline state. `tests/upscale.rs`
checks the built-in scales, subpixel coverage/interpolation, transfers and masks.
The browser suite checks golden vectors, upscaling and context recovery.
