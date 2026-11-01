# Super-xBR 3p sprite pipeline

Ports the full `super-xbr-3p-smoother.cgp` pipeline from
https://github.com/libretro/common-shaders/tree/master/xbr :

1. `super-xbr-pass0.cg`: native-size diagonal reconstruction.
2. `super-xbr-pass1.cg`: 2× reconstruction using pass 0 and the original input.
3. `super-xbr-pass2.cg`: smoothing at 2×.
4. `custom-jinc2-sharper.cg`: final resampling, fused into primitive shading.

Defaults follow that Cg preset: edge strength 2, filter weight 1, anti-ringing 1;
Jinc window .42, sinc .92 and anti-ringing 1. This is not the newer GLSL variant
with different defaults/edge weighting. The core passes use RGBA8 intermediates.

PSX-specific extensions decode read-only native textures/CLUTs on the GPU, clamp
sampling to each sprite's texel footprint, filter premultiplied colour/coverage,
and keep separate opaque/STP planes for semitransparent primitives. Final shading
retains modulation, dithering, destination blending and mask checks. Non-sprite
geometry still uses the independent polygon texture setting. No full-frame filter,
game/adaptor changes, canonical writes or CPU image readbacks are involved.

The tile cache persists across draws/frames, bounded to 16 MiB and 256 entries.
SYNC invalidates overlapping physical source words after texture-window mapping.
Paletted images use the loaded palette snapshot's provenance/version, not the
rotating CLUT slot number. Reloads from unchanged read-VRAM regions can reuse
processed images across slots; canonical CLUT_LOAD commands are never suppressed.
Separate 16-entry/full-table identities preserve partial-load tails. Palette VRAM
writes alone do not alter already-loaded snapshots, and unknown initial slots
remain distinct when installing a factory mid-game. Provenance tracking is bounded
to 4096 source regions; dropping metadata only loses reuse. Clipped-row callbacks conservatively invalidate the
cache before and after execution because internal serial feedback can synchronize
native read VRAM between those callbacks. Reset invalidates all cached history.
Ordinary geometry and adjacent sprites sharing a tile retain instanced batching.
Visual destination hazards use coalesced written rectangles, not only their combined
bounds, so disjoint tile rows do not force intermediate copies. Tracking is bounded
to 64 regions, with a conservative union fallback; real overlaps still synchronize.
The final Jinc kernel skips its expensive work when its anti-ringing range is
constant: this gives exactly the same clamped result, not a cheaper filter.

## Licenses and source provenance

`pass.glsl` derives from Hyllian's MIT-licensed core passes; its notice is retained.
`jinc.glsl` derives from Hyllian/Jararaca's **GPL-2.0-or-later** Jinc shader; its
notice is retained, and `COPYING.GPL-2` contains the license text. Broader
licensing/distribution review remains pending. This notice does not resolve those
obligations or change the workspace's existing license declaration.

Source SHA-256 hashes (under `xbr/shaders/super-xbr/`):

- `super-xbr-pass0.cg`: `c861c7d4ae3bbc827c591afb26515e9a8f25610743185349b344384e9e6a38b6`
- `super-xbr-pass1.cg`: `99e6b8a9cee6610b829b382452392c4a63d37b5be0c8ab680e94e58419de7926`
- `super-xbr-pass2.cg`: `b707786dbfb6d405058c410162ba834e1d090ca6b20317b0fcd85a2e6025542e`
- `custom-jinc2-sharper.cg`: `230e57b5c8c72830c14a45fec62c4ca5dd1599a2dae9c334bc0dc82b2db719ef`
