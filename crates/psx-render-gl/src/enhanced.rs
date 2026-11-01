//! Built-in upscaled visual renderer. All enhanced setup, resources, hazards
//! and presentation live here; the exact VRAM engine knows only VisualRenderer.

use anyhow::{Result, ensure};
use glow::HasContext;
use psxw_abi::render::{Command, PRIM_WORDS, PresentDesc, prim, unpack_xy};

use crate::{
    Stats, framebuffer,
    presentation::Presenter,
    raster::{PER_DRAW, RasterPass, RasterTarget, UploadBatch},
    setup::{self, Clip, WORDS},
    shaders::{self, DrawShader},
    texture,
    visual::{ColourPrecision, TextureFilter, VisualContext, VisualFrame, VisualRenderer},
};

pub(crate) struct Enhanced {
    scale: i32,
    dithering: bool,
    high_precision: bool,
    draw: glow::Texture,
    read: glow::Texture,
    fbo_draw: glow::Framebuffer,
    fbo_read: glow::Framebuffer,
    stage: glow::Texture,
    raster: RasterPass,
    presenter: Presenter,
    staged: UploadBatch,
    scratch: Vec<u32>,
    stats: Stats,
    // Inclusive native-coordinate bounds not yet in the enhanced read plane.
    dirty: Option<[i32; 4]>,
}

impl Enhanced {
    pub fn new(ctx: &VisualContext<'_>, scale: i32, filter: TextureFilter, dithering: bool, precision: ColourPrecision) -> Result<Self> {
        let gl = ctx.gl;
        let high_precision = precision == ColourPrecision::High;
        let format = if high_precision { glow::R32UI } else { glow::R16UI };
        unsafe {
            let limit = gl.get_parameter_i32(glow::MAX_TEXTURE_SIZE);
            let mut viewport = [0; 2];
            gl.get_parameter_i32_slice(glow::MAX_VIEWPORT_DIMS, &mut viewport);
            ensure!(
                1024 * scale <= limit && 1024 * scale <= viewport[0] && 512 * scale <= viewport[1],
                "{scale}× rendering exceeds this GPU's limits"
            );
            let (mut textures, mut fbos) = (Vec::new(), Vec::new());
            let result = (|| {
                let draw = texture(gl, format, 1024 * scale, 512 * scale)?;
                textures.push(draw);
                let read = texture(gl, format, 1024 * scale, 512 * scale)?;
                textures.push(read);
                let fbo_draw = framebuffer(gl, draw)?;
                fbos.push(fbo_draw);
                let fbo_read = framebuffer(gl, read)?;
                fbos.push(fbo_read);
                let stage = texture(gl, glow::R16UI, 1024, 512)?;
                textures.push(stage);
                let triangle = format!("const bool SMOOTH_3D = {};\n{TEXTURE_FILTER}\n{TRIANGLE}", filter == TextureFilter::Smooth3D);
                let fragment = shaders::draw_fs_with(&DrawShader {
                    colour: if high_precision { HIGH_COLOUR } else { shaders::NATIVE_COLOUR },
                    triangle: &triangle,
                    triangle_fn: "enhanced_triangle",
                    position: "int px = hp.x / u_scale, py = hp.y / u_scale;",
                    destination: "texelFetch(u_dest, hp, 0).r",
                    copy_source: "texelFetch(u_dest, ivec2((int(m.x) + i) & 1023, (int(m.y) + j) & 511) * u_scale + hp % u_scale, 0).r",
                });
                let mut raster = RasterPass::new(gl, &fragment)?;
                let mut presenter = match Presenter::new(gl) {
                    Ok(p) => p,
                    Err(e) => {
                        raster.destroy(gl);
                        return Err(e);
                    }
                };
                for f in [fbo_draw, fbo_read] {
                    if high_precision {
                        // A blit cannot expand RGB5/mask into RGB8/mask. Import
                        // canonical words through this renderer's colour shader,
                        // without CPU readback or binding an output as a source.
                        let mut rec = vec![0; WORDS];
                        setup::upload(0, 0, 1024, 512, 0, 0, 0, &mut rec);
                        raster.bind(gl, ctx.vram, ctx.vram.draw, ctx.state, RasterTarget {
                            framebuffer: f, scale, destination: Some(ctx.vram.draw),
                        });
                        raster.upload(gl, &mut rec, 0, 1);
                        raster.draw(gl, 0, 1);
                    } else {
                        ctx.vram.seed(gl, f, scale);
                    }
                }
                if gl.get_error() != glow::NO_ERROR {
                    raster.destroy(gl);
                    presenter.destroy(gl);
                    anyhow::bail!("allocating {scale}× rendering failed");
                }
                Ok(Self {
                    scale,
                    dithering,
                    high_precision,
                    draw,
                    read,
                    fbo_draw,
                    fbo_read,
                    stage,
                    raster,
                    presenter,
                    staged: UploadBatch::default(),
                    scratch: Vec::new(),
                    stats: Stats::default(),
                    dirty: None,
                })
            })();
            if result.is_err() {
                for f in fbos {
                    gl.delete_framebuffer(f);
                }
                for t in textures {
                    gl.delete_texture(t);
                }
            }
            result
        }
    }

    fn draw_records(&mut self, ctx: &VisualContext<'_>, words: &mut Vec<u32>, n: usize) {
        let gl = ctx.gl;
        self.raster.bind(
            gl,
            ctx.vram,
            self.stage,
            ctx.state,
            RasterTarget { framebuffer: self.fbo_draw, scale: self.scale, destination: Some(self.read) },
        );
        let mut start = 0;
        while start < n {
            let m = PER_DRAW.min(n - start);
            self.raster.upload(gl, words, start, m);
            let mut run = 0;
            for i in 0..m {
                let rec = &words[(start + i) * WORDS..(start + i + 1) * WORDS];
                if self.needs_sync(rec) {
                    if i > run {
                        self.raster.draw(gl, run, i - run);
                        self.stats.draws += 1;
                    }
                    self.sync(gl);
                    run = i;
                }
                self.written(rec);
            }
            self.raster.draw(gl, run, m - run);
            self.stats.draws += 1;
            start += m;
        }
    }

    fn needs_sync(&self, rec: &[u32]) -> bool {
        let Some(d) = self.dirty else { return false };
        let kind = rec[0] & prim::KIND_MASK;
        if kind == setup::COPY {
            return true;
        }
        let reads_dest =
            if kind == setup::UPLOAD { rec[18] & 2 != 0 } else { rec[0] & (prim::SEMI | prim::MASK_CHECK) != 0 };
        let b = &rec[8..12];
        reads_dest
            && b[0] <= b[2]
            && b[1] <= b[3]
            && d[0] <= b[2] as i32
            && d[2] >= b[0] as i32
            && d[1] <= b[3] as i32
            && d[3] >= b[1] as i32
    }

    fn written(&mut self, rec: &[u32]) {
        let b = &rec[8..12];
        if b[0] > b[2] || b[1] > b[3] {
            return;
        }
        let b = [b[0] as i32, b[1] as i32, b[2] as i32, b[3] as i32];
        self.dirty = Some(self.dirty.map_or(b, |d| [d[0].min(b[0]), d[1].min(b[1]), d[2].max(b[2]), d[3].max(b[3])]));
    }

    fn sync(&mut self, gl: &glow::Context) {
        let Some([x0, y0, x1, y1]) = self.dirty.take() else { return };
        let s = self.scale;
        unsafe {
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(self.fbo_draw));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(self.fbo_read));
            gl.blit_framebuffer(
                x0 * s,
                y0 * s,
                (x1 + 1) * s,
                (y1 + 1) * s,
                x0 * s,
                y0 * s,
                (x1 + 1) * s,
                (y1 + 1) * s,
                glow::COLOR_BUFFER_BIT,
                glow::NEAREST,
            );
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(self.fbo_draw));
        }
        self.stats.syncs += 1;
    }
}

impl VisualRenderer for Enhanced {
    fn execute(&mut self, ctx: &VisualContext<'_>, command: Command<'_>) -> Result<()> {
        match command {
            Command::Draw(recs) => {
                let n = recs.len() / PRIM_WORDS;
                let mut scratch = std::mem::take(&mut self.scratch);
                scratch.clear();
                scratch.resize(n * WORDS, 0);
                for (i, rec) in recs.chunks_exact(PRIM_WORDS).enumerate() {
                    primitive(rec, &ctx.state.clip, &mut scratch[i * WORDS..(i + 1) * WORDS]);
                    if !self.dithering {
                        scratch[i * WORDS] &= !prim::DITHER;
                    }
                }
                self.draw_records(ctx, &mut scratch, n);
                self.scratch = scratch;
            }
            Command::Fill { rect, color } => {
                let mut rec = vec![0; WORDS];
                setup::fill(rect.x, rect.y, rect.w, rect.h, color, &mut rec);
                self.draw_records(ctx, &mut rec, 1);
            }
            Command::Upload { rect, mask, pixels } => {
                if !self.staged.fits(rect) {
                    self.flush(ctx)?;
                }
                self.staged.push(rect, mask, pixels);
                self.stats.uploads += 1;
            }
            Command::Copy { src_x, src_y, dst_x, dst_y, w, h, mask } => {
                let mut rec = vec![0; WORDS];
                setup::copy((src_x, src_y), (dst_x, dst_y), w, h, mask, &mut rec);
                self.draw_records(ctx, &mut rec, 1);
            }
            _ => {}
        }
        Ok(())
    }

    fn flush(&mut self, ctx: &VisualContext<'_>) -> Result<()> {
        if self.staged.count != 0 {
            let mut staged = std::mem::take(&mut self.staged);
            staged.upload(ctx.gl, self.stage);
            self.draw_records(ctx, &mut staged.records, staged.count);
            staged.clear();
            self.staged = staged;
        }
        Ok(())
    }

    fn reset(&mut self, ctx: &VisualContext<'_>) {
        unsafe {
            for f in [self.fbo_draw, self.fbo_read] {
                ctx.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(f));
                ctx.gl.clear_buffer_u32_slice(glow::COLOR, 0, &[0, 0, 0, 0]);
            }
        }
        self.dirty = None;
        self.staged.clear();
        self.presenter.reset(ctx.gl);
    }

    fn present(&mut self, ctx: &VisualContext<'_>, desc: &PresentDesc) -> Result<VisualFrame> {
        // Packed 24-bit video is byte-addressed, not polygon colour data.
        let (source, scale) =
            if desc.flags & PresentDesc::DEPTH24 != 0 { (ctx.vram.draw, 1) } else { (self.draw, self.scale as u32) };
        self.presenter.present_with_precision(ctx.gl, source, scale, desc, self.high_precision && desc.flags & PresentDesc::DEPTH24 == 0)
    }

    fn take_stats(&mut self) -> Stats {
        std::mem::take(&mut self.stats)
    }

    fn destroy(&mut self, gl: &glow::Context) {
        self.raster.destroy(gl);
        self.presenter.destroy(gl);
        unsafe {
            gl.delete_framebuffer(self.fbo_draw);
            gl.delete_framebuffer(self.fbo_read);
            gl.delete_texture(self.draw);
            gl.delete_texture(self.read);
            gl.delete_texture(self.stage);
        }
    }
}

// Ordinary polygons use raw vertices; unusual wrapping keeps native coverage.
// This packed record layout is private to this implementation and its shader.
fn primitive(rec: &[u32], clip: &Clip, out: &mut [u32]) {
    setup::primitive(rec, clip, out);
    let kind = rec[0] & prim::KIND_MASK;
    if kind != prim::TRIANGLE && kind != prim::QUAD {
        return;
    }
    // Sprite quads often end their UVs just past an atlas tile. Native draws
    // never sample that endpoint, but subpixel interpolation can. Preserve the
    // native texel footprint for axis-aligned screen/UV rectangles, including
    // flipped or rotated UV axes. Read coefficients before replacing the setup.
    let sprite_bounds = if kind == prim::QUAD && rec[0] & prim::TEXTURED != 0 {
        let xy: [_; 4] = std::array::from_fn(|n| unpack_xy(rec[3 + 3 * n]));
        let uv: [_; 4] = std::array::from_fn(|n| {
            let t = rec[5 + 3 * n];
            ((t & 255) as i32, ((t >> 8) & 255) as i32)
        });
        let rectangle = |p: [(i32, i32); 4]| {
            let a = (p[1].0 - p[0].0, p[1].1 - p[0].1);
            let b = (p[2].0 - p[0].0, p[2].1 - p[0].1);
            ((a.0 == 0 && b.1 == 0) || (a.1 == 0 && b.0 == 0))
                && p[3] == (p[1].0 + p[2].0 - p[0].0, p[1].1 + p[2].1 - p[0].1)
        };
        if rectangle(xy) && rectangle(uv) {
            let x0 = xy.iter().map(|p| p.0).min().unwrap();
            let x1 = xy.iter().map(|p| p.0).max().unwrap() - 1;
            let y0 = xy.iter().map(|p| p.1).min().unwrap();
            let y1 = xy.iter().map(|p| p.1).max().unwrap() - 1;
            let mut bounds = [255, 255, 0, 0];
            for at in [3 * 4, 14 * 4] {
                for (x, y) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
                    for axis in 0..2 {
                        let t = out[at + 28 + axis]
                            .wrapping_add(out[at + 33 + axis].wrapping_mul(x as u32))
                            .wrapping_add(out[at + 38 + axis].wrapping_mul(y as u32)) >> 24;
                        bounds[axis] = bounds[axis].min(t);
                        bounds[axis + 2] = bounds[axis + 2].max(t);
                    }
                }
            }
            Some(bounds)
        } else { None }
    } else { None };
    for (t, indices) in [[0, 1, 2], [1, 2, 3]].into_iter().enumerate() {
        if t == 1 && kind != prim::QUAD {
            break;
        }
        let at = if t == 0 { 3 * 4 } else { 14 * 4 };
        let vertices = indices.map(|n| unpack_xy(rec[3 + 3 * n]));
        if out[at] == 0 || !vertices.iter().all(|&(x, y)| (-1024..1024).contains(&x) && (-1024..1024).contains(&y)) {
            continue;
        }
        out[2] |= 1 << t;
        for (i, n) in indices.into_iter().enumerate() {
            let (x, y) = vertices[i];
            out[at + 4 * i..at + 4 * i + 4].copy_from_slice(&[x as u32, y as u32, rec[4 + 3 * n], rec[5 + 3 * n]]);
        }
    }
    // T(6) is unused only when both triangles use enhanced vertex records.
    // Leave unusual/wrapping native fallbacks completely intact.
    if out[2] & 3 == 3 && let Some(bounds) = sprite_bounds {
        out[2] |= 4;
        out[6 * 4..7 * 4].copy_from_slice(&bounds);
    }
}

// A packed RGB8 visual plane, with a separate mask bit. Native textures and
// uploads expand exactly (31 -> 248), but new shading/filtering/blending keeps
// eight bits per channel rather than repeatedly rounding back to RGB5.
const HIGH_COLOUR: &str = r#"
const uint PIXEL_MASK = 0x80000000u;
vec3 rgb8(uint p) { return vec3(p & 255u, (p >> 8) & 255u, (p >> 16) & 255u); }
uint colour_pixel(vec3 c, uint mask) {
    uvec3 v = uvec3(clamp(floor(c + 0.5), 0.0, 255.0));
    return mask | v.r | (v.g << 8) | (v.b << 16);
}
uint native_pixel(uint p) {
    return colour_pixel(vec3(p & 31u, (p >> 5) & 31u, (p >> 10) & 31u) * 8.0,
                        (p & 0x8000u) != 0u ? PIXEL_MASK : 0u);
}
uint blend(uint bg, uint fg, uint abr) {
    vec3 b = rgb8(bg), f = rgb8(fg);
    vec3 c = abr == 0u ? (b + f) * 0.5 : abr == 1u ? b + f : abr == 2u ? b - f : b + f * 0.25;
    return colour_pixel(c, fg & PIXEL_MASK);
}
uint modulate(uint t, uint r, uint g, uint b, int d) {
    return colour_pixel(rgb8(t) * vec3(r, g, b) / 128.0 + float(d), t & PIXEL_MASK);
}
uint shade(uint r, uint g, uint b, bool dither, int d) {
    return colour_pixel(vec3(r, g, b) + float(dither ? d : 0), PIXEL_MASK);
}
"#;

// Filter decoded colours, never palette indices or the mask bit. A nearest
// sample controls transparency/blend classification; incompatible neighbours
// are excluded and weights renormalized, avoiding black fringes and new holes.
const TEXTURE_FILTER: &str = r#"
vec3 rgb5(uint t) { return vec3(t & 31u, (t >> 5) & 31u, (t >> 10) & 31u); }
void polygon_surface(vec2 at, vec2 lo, vec2 hi, uvec3 color, uint flags, uvec4 h0, uvec4 tw, int d,
                     inout uint dst, inout bool wrote) {
    ivec2 nearest = ivec2(floor(at + 0.5));
    if (!SMOOTH_3D || (flags & TEXTURED) == 0u) {
        surface(uint(nearest.x) & 255u, uint(nearest.y) & 255u, color.r, color.g, color.b, flags, h0, tw, d, dst, wrote);
        return;
    }
    uint t = texel(uint(nearest.x) & 255u, uint(nearest.y) & 255u, flags, tw, h0.y >> 16);
    if (t == 0u) return;
    vec2 base = floor(at), f = fract(at);
    vec3 sum = vec3(0.0);
    float weight = 0.0;
    for (int y = 0; y < 2; y++) {
        for (int x = 0; x < 2; x++) {
            // Conservative atlas guard: never fetch outside this triangle's UV
            // bounds. Texture windows/page wrapping still apply in texel().
            ivec2 p = ivec2(clamp(base + vec2(x, y), lo, hi));
            uint n = texel(uint(p.x) & 255u, uint(p.y) & 255u, flags, tw, h0.y >> 16);
            if (n == 0u || ((n ^ t) & 0x8000u) != 0u) continue;
            float w = (x == 0 ? 1.0 - f.x : f.x) * (y == 0 ? 1.0 - f.y : f.y);
            sum += rgb5(n) * w;
            weight += w;
        }
    }
    uint mask = native_pixel(t) & PIXEL_MASK;
    t = weight > 0.0 ? colour_pixel(sum / weight * 8.0, mask) : native_pixel(t);
    if ((flags & RAW) == 0u) t = modulate(t, color.r, color.g, color.b, (flags & DITHER) != 0u ? d : 0);
    plot(t, true, flags, dst, wrote);
}
"#;

const TRIANGLE: &str = r#"
float edge(vec2 a, vec2 b, vec2 p) {
    vec2 d = b - a, q = p - a;
    return d.x * q.y - d.y * q.x;
}
bool inside_edge(float e, vec2 a, vec2 b) {
    vec2 d = b - a;
    return e > 0.0 || (e == 0.0 && (d.y < 0.0 || (d.y == 0.0 && d.x > 0.0)));
}
vec3 rgb(uint c) { return vec3(c & 255u, (c >> 8) & 255u, (c >> 16) & 255u); }
vec2 uv(uint t) { return vec2(t & 255u, (t >> 8) & 255u); }
void enhanced_triangle(int b, int px, int py, uint flags, uvec4 h0, uvec4 tw, inout uint dst, inout bool wrote) {
    uint bit = b == 3 ? 1u : 2u;
    if ((h0.z & bit) == 0u) { triangle(b, px, py, flags, h0, tw, dst, wrote); return; }
    uvec4 va = T(b), vb = T(b + 1), vc = T(b + 2);
    vec2 a = vec2(ivec2(va.xy)), bb = vec2(ivec2(vb.xy)), c = vec2(ivec2(vc.xy));
    float area = edge(a, bb, c);
    if (area < 0.0) {
        uvec4 tmp = vb; vb = vc; vc = tmp;
        vec2 vtmp = bb; bb = c; c = vtmp;
        area = -area;
    }
    int y0 = int(min(a.y, min(bb.y, c.y)));
    int yi = y0 + ((py - y0) & 511);
    int sy = sext11(yi);
    if (sy < u_clip.y || sy > u_clip.w || skipped(yi)) return;
    vec2 sub = vec2(ivec2(gl_FragCoord.xy) % u_scale) / float(u_scale);
    vec2 p = vec2(px, yi) + sub;
    float ea = edge(bb, c, p), eb = edge(c, a, p), ec = edge(a, bb, p);
    if (!inside_edge(ea, bb, c) || !inside_edge(eb, c, a) || !inside_edge(ec, a, bb)) return;
    vec3 w = vec3(ea, eb, ec) / area;
    vec3 color = clamp(floor(rgb(va.z) * w.x + rgb(vb.z) * w.y + rgb(vc.z) * w.z + 0.5), 0.0, 255.0);
    vec2 ta = uv(va.w), tb = uv(vb.w), tc = uv(vc.w);
    vec2 t = ta * w.x + tb * w.y + tc * w.z;
    vec2 lo = min(ta, min(tb, tc)), hi = max(ta, max(tb, tc));
    if ((h0.z & 4u) != 0u) {
        // Centre the subpixel samples around the native sprite pixel, rather
        // than shifting texel boundaries halfway into the preceding pixel.
        vec2 q = p + vec2(0.5 / float(u_scale) - 0.5);
        vec3 tex_w = vec3(edge(bb, c, q), edge(c, a, q), edge(a, bb, q)) / area;
        uvec4 bounds = T(6);
        lo = vec2(bounds.xy); hi = vec2(bounds.zw);
        t = clamp(ta * tex_w.x + tb * tex_w.y + tc * tex_w.z, lo, hi);
    }
    polygon_surface(t, lo, hi, uvec3(color), flags, h0, tw, dither_at(px, py), dst, wrote);
}
"#;
