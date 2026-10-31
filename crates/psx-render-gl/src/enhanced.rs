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
    visual::{VisualContext, VisualFrame, VisualRenderer},
};

pub(crate) struct Enhanced {
    scale: i32,
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
    pub fn new(ctx: &VisualContext<'_>, scale: i32) -> Result<Self> {
        let gl = ctx.gl;
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
                let draw = texture(gl, glow::R16UI, 1024 * scale, 512 * scale)?;
                textures.push(draw);
                let read = texture(gl, glow::R16UI, 1024 * scale, 512 * scale)?;
                textures.push(read);
                let fbo_draw = framebuffer(gl, draw)?;
                fbos.push(fbo_draw);
                let fbo_read = framebuffer(gl, read)?;
                fbos.push(fbo_read);
                let stage = texture(gl, glow::R16UI, 1024, 512)?;
                textures.push(stage);
                let fragment = shaders::draw_fs_with(&DrawShader {
                    triangle: TRIANGLE,
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
                    ctx.vram.seed(gl, f, scale);
                }
                if gl.get_error() != glow::NO_ERROR {
                    raster.destroy(gl);
                    presenter.destroy(gl);
                    anyhow::bail!("allocating {scale}× rendering failed");
                }
                Ok(Self {
                    scale,
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
        self.presenter.present(ctx.gl, source, scale, desc)
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
}

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
    ivec2 t = ivec2(floor(uv(va.w) * w.x + uv(vb.w) * w.y + uv(vc.w) * w.z + 0.5));
    surface(uint(t.x) & 255u, uint(t.y) & 255u, uint(color.r), uint(color.g), uint(color.b), flags, h0, tw, dither_at(px, py), dst, wrote);
}
"#;
