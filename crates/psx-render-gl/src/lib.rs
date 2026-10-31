//! The GPU backend (M4; docs/design.md "Runtime", gpu; decision D20):
//! executes the renderer command stream of [`psxw_abi::render`] over
//! [`glow`], so the same code drives WebGL2 in the browser and GLES 3.0
//! natively (headless through EGL surfaceless with the `headless`
//! feature).
//!
//! VRAM is a 1024×512 `R16UI` render target holding the exact 16-bit
//! pixels (the *draw* copy), plus an `R16UI` texture the shaders read (the
//! *read* copy), brought up to date region by region by SYNC commands.
//! Each DRAW batch is one instanced draw of covering rectangles; the
//! fragment shader decides coverage and computes the pixel in integers,
//! as the PS1 GPU does at 1× (see [`setup`] and [`shaders`]).
//!
//! The backend is a pure function from commands and VRAM to VRAM; the
//! front end (`psx_devices::gpu`) owns all PS1 state and native hazards.
//! Visual rendering is delegated through [`visual::VisualRenderer`]; the exact
//! VRAM engine alone supplies game readbacks and canonical texture/CLUT data.

mod enhanced;
mod presentation;
mod raster;
pub mod visual;
pub mod server;
pub mod setup;
pub mod shaders;

#[cfg(feature = "headless")]
pub mod headless;

use anyhow::{Context as _, Result, bail, ensure};
use glow::HasContext;
use psxw_abi::render::{self, Command, PRIM_WORDS, PresentDesc};
use setup::{Clip, WORDS};
use raster::{PER_DRAW, RasterPass, RasterTarget, UploadBatch};
use visual::{CanonicalVram, DrawState, VisualContext, VisualFrame, VisualRenderer};

/// Version of this backend, for reports.
pub const BACKEND_VERSION: &str = concat!("psx-render-gl ", env!("CARGO_PKG_VERSION"), " render-v1");

/// Strings the GL implementation reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlInfo {
    pub vendor: String,
    pub renderer: String,
    pub version: String,
    pub glsl: String,
}

/// A rendered frame, RGBA8, row 0 at the top.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The backend: VRAM on the GPU and the programs that draw into it.
pub struct Renderer {
    gl: glow::Context,
    vram_w: glow::Texture,
    vram_r: glow::Texture,
    fbo_w: glow::Framebuffer,
    fbo_r: glow::Framebuffer,
    clut: glow::Texture,
    fbo_clut: glow::Framebuffer,
    stage: glow::Texture,
    raster: RasterPass,
    visual: Box<dyn VisualRenderer>,
    preset_scale: Option<u32>,
    last_frame: Option<VisualFrame>,
    read_format: (u32, u32, usize),
    clip: Clip,
    skip: i32,
    readbacks: Vec<u16>,
    scratch: Vec<u32>,
    staged: UploadBatch,
    last_present: Option<PresentDesc>,
    /// GL work since [`Renderer::take_stats`].
    stats: Stats,
}

/// What the backend did, for hosts that report where a frame's time went:
/// draw calls, syncs (copies to the read copy, each its own render pass),
/// rows of primitives drawn one by one, uploads, colour-table loads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub draws: u32,
    pub syncs: u32,
    pub serial_rows: u32,
    pub uploads: u32,
    pub clut_loads: u32,
}

impl Stats {
    fn include(&mut self, other: Self) {
        self.draws += other.draws;
        self.syncs += other.syncs;
        self.serial_rows += other.serial_rows;
        self.uploads += other.uploads;
        self.clut_loads += other.clut_loads;
    }
}

fn gl_err(e: String) -> anyhow::Error {
    anyhow::anyhow!(e)
}

unsafe fn texture(gl: &glow::Context, internal: u32, w: i32, h: i32) -> Result<glow::Texture> {
    unsafe {
        let t = gl.create_texture().map_err(gl_err)?;
        gl.bind_texture(glow::TEXTURE_2D, Some(t));
        gl.tex_storage_2d(glow::TEXTURE_2D, 1, internal, w, h);
        // Integer textures are only complete with NEAREST filtering.
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::NEAREST as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::NEAREST as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
        let error = gl.get_error();
        if error != glow::NO_ERROR {
            gl.delete_texture(t);
            bail!("allocating {w}×{h} texture failed: GL error {error:#x}");
        }
        Ok(t)
    }
}

unsafe fn framebuffer(gl: &glow::Context, tex: glow::Texture) -> Result<glow::Framebuffer> {
    unsafe {
        let f = gl.create_framebuffer().map_err(gl_err)?;
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(f));
        gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(tex), 0);
        let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
        if status != glow::FRAMEBUFFER_COMPLETE {
            gl.delete_framebuffer(f);
            gl.get_error();
            bail!("framebuffer incomplete: {status:#x}");
        }
        Ok(f)
    }
}

unsafe fn program(gl: &glow::Context, vs: &str, fs: &str) -> Result<glow::Program> {
    unsafe {
        let p = gl.create_program().map_err(gl_err)?;
        let mut shaders = Vec::new();
        for (kind, src) in [(glow::VERTEX_SHADER, vs), (glow::FRAGMENT_SHADER, fs)] {
            let s = gl.create_shader(kind).map_err(gl_err)?;
            gl.shader_source(s, src);
            gl.compile_shader(s);
            if !gl.get_shader_compile_status(s) {
                bail!("shader compile failed: {}", gl.get_shader_info_log(s));
            }
            gl.attach_shader(p, s);
            shaders.push(s);
        }
        gl.link_program(p);
        if !gl.get_program_link_status(p) {
            bail!("program link failed: {}", gl.get_program_info_log(p));
        }
        for s in shaders {
            gl.detach_shader(p, s);
            gl.delete_shader(s);
        }
        Ok(p)
    }
}

/// Split a wrapping VRAM rectangle into up to four that don't wrap.
fn pieces(x: u32, y: u32, w: u32, h: u32) -> impl Iterator<Item = (u32, u32, u32, u32, u32, u32)> {
    // (x, y, w, h, offset into the rectangle x, y)
    let xs = if x + w <= 1024 { [(x, w, 0), (0, 0, 0)] } else { [(x, 1024 - x, 0), (0, x + w - 1024, 1024 - x)] };
    let ys = if y + h <= 512 { [(y, h, 0), (0, 0, 0)] } else { [(y, 512 - y, 0), (0, y + h - 512, 512 - y)] };
    xs.into_iter()
        .flat_map(move |xp| ys.into_iter().map(move |yp| (xp, yp)))
        .filter(|((_, w, _), (_, h, _))| *w > 0 && *h > 0)
        .map(|((x, w, ox), (y, h, oy))| (x, y, w, h, ox, oy))
}

impl Renderer {
    /// Set up VRAM and the programs on a current GLES 3.0 / WebGL2
    /// context.
    pub fn new(gl: glow::Context) -> Result<Self> {
        unsafe {
            let version = gl.version();
            ensure!(version.is_embedded && version.major >= 3 || !version.is_embedded && version.major >= 4, "need GLES 3.0 / WebGL2, got {version:?}");
            let vram_w = texture(&gl, glow::R16UI, 1024, 512)?;
            let vram_r = texture(&gl, glow::R16UI, 1024, 512)?;
            let clut = texture(&gl, glow::R16UI, 256, render::CLUT_SLOTS as i32)?;
            let stage = texture(&gl, glow::R16UI, 1024, 512)?;
            let fbo_w = framebuffer(&gl, vram_w)?;
            let fbo_r = framebuffer(&gl, vram_r)?;
            let fbo_clut = framebuffer(&gl, clut)?;
            let raster = RasterPass::new(&gl, &shaders::draw_fs())?;
            let visual = visual::create(&VisualContext {
                gl: &gl,
                vram: CanonicalVram { draw: vram_w, read: vram_r, clut, framebuffer: fbo_w },
                state: DrawState { clip: Clip { x0: 0, y0: 0, x1: 0, y1: 0 }, skip: -1 },
            }, 1)?;
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            gl.disable(glow::BLEND);
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::DITHER);
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(fbo_w));
            let f = gl.get_parameter_i32(glow::IMPLEMENTATION_COLOR_READ_FORMAT) as u32;
            let t = gl.get_parameter_i32(glow::IMPLEMENTATION_COLOR_READ_TYPE) as u32;
            let read_format = if f == glow::RED_INTEGER && t == glow::UNSIGNED_SHORT {
                (f, t, 2)
            } else {
                (glow::RGBA_INTEGER, glow::UNSIGNED_INT, 16)
            };
            let mut r = Renderer {
                gl,
                vram_w,
                vram_r,
                fbo_w,
                fbo_r,
                clut,
                fbo_clut,
                stage,
                raster,
                visual,
                preset_scale: Some(1),
                last_frame: None,
                read_format,
                clip: Clip { x0: 0, y0: 0, x1: 0, y1: 0 },
                skip: -1,
                readbacks: Vec::new(),
                scratch: Vec::new(),
                staged: UploadBatch::default(),
                last_present: None,
                stats: Stats::default(),
            };
            r.reset_vram();
            let err = r.gl.get_error();
            ensure!(err == glow::NO_ERROR, "GL error {err:#x} during setup");
            Ok(r)
        }
    }

    /// Host-local internal resolution. This never changes the command ABI or
    /// the native VRAM returned to the game. Changing scale seeds the new colour
    /// plane from native VRAM; detail returns as the game redraws it.
    pub fn set_resolution_scale(&mut self, scale: u32) -> Result<()> {
        ensure!(matches!(scale, 1 | 2 | 4), "resolution scale must be 1, 2 or 4");
        if self.preset_scale == Some(scale) { return Ok(()); }
        self.set_visual_renderer(|ctx| visual::create(ctx, scale))?;
        self.preset_scale = Some(scale);
        Ok(())
    }

    /// Install a host-local visual renderer without changing the exact engine.
    /// Build transactionally from read-only canonical inputs; failure retains
    /// the old visual renderer and output. Recreate the factory after context loss.
    pub fn set_visual_renderer(&mut self, factory: impl FnOnce(&VisualContext<'_>) -> Result<Box<dyn VisualRenderer>>) -> Result<()> {
        self.draw_staged();
        self.flush_visual()?;
        let next = factory(&VisualContext { gl: &self.gl, vram: self.canonical_vram(), state: self.draw_state() })?;
        let mut old = std::mem::replace(&mut self.visual, next);
        self.stats.include(old.take_stats());
        old.destroy(&self.gl);
        self.last_frame = None;
        self.preset_scale = None;
        Ok(())
    }

    /// Built-in scale selection; custom visual renderers report 1.
    pub fn resolution_scale(&self) -> u32 { self.preset_scale.unwrap_or(1) }

    /// The last presentation's backing resolution, not the native VRAM size.
    pub fn output_size(&self) -> Option<(u32, u32)> {
        self.last_frame.map(|f| (f.width, f.height))
    }

    fn canonical_vram(&self) -> CanonicalVram {
        CanonicalVram { draw: self.vram_w, read: self.vram_r, clut: self.clut, framebuffer: self.fbo_w }
    }

    fn draw_state(&self) -> DrawState { DrawState { clip: self.clip, skip: self.skip } }

    fn visual_command(&mut self, command: Command<'_>) -> Result<()> {
        let (vram, state) = (self.canonical_vram(), self.draw_state());
        self.visual.execute(&VisualContext { gl: &self.gl, vram, state }, command)
    }

    fn flush_visual(&mut self) -> Result<()> {
        let (vram, state) = (self.canonical_vram(), self.draw_state());
        self.visual.flush(&VisualContext { gl: &self.gl, vram, state })
    }

    /// What the GL implementation says it is.
    pub fn info(&self) -> GlInfo {
        unsafe {
            GlInfo {
                vendor: self.gl.get_parameter_string(glow::VENDOR),
                renderer: self.gl.get_parameter_string(glow::RENDERER),
                version: self.gl.get_parameter_string(glow::VERSION),
                glsl: self.gl.get_parameter_string(glow::SHADING_LANGUAGE_VERSION),
            }
        }
    }

    /// Zero VRAM (both copies) and the colour-table slots, and forget the
    /// batch state.
    pub fn reset_vram(&mut self) {
        raster::prepare_draw(&self.gl);
        unsafe {
            for f in [self.fbo_w, self.fbo_r, self.fbo_clut] {
                self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(f));
                self.gl.draw_buffers(&[glow::COLOR_ATTACHMENT0]);
                self.gl.clear_buffer_u32_slice(glow::COLOR, 0, &[0, 0, 0, 0]);
            }
        }
        self.clip = Clip { x0: 0, y0: 0, x1: 0, y1: 0 };
        self.skip = -1;
        self.staged.clear();
        self.readbacks.clear();
        let (vram, state) = (self.canonical_vram(), self.draw_state());
        self.visual.reset(&VisualContext { gl: &self.gl, vram, state });
        self.last_frame = None;
    }

    /// Execute a command stream. READBACK results accumulate for
    /// [`Renderer::take_readbacks`].
    pub fn execute(&mut self, words: &[u32]) -> Result<()> {
        for cmd in render::commands(words) {
            let cmd = cmd.map_err(|e| anyhow::anyhow!("bad command stream: {e:?}"))?;
            // Uploads in a row draw as one batch.
            if !matches!(cmd, Command::Upload { .. }) {
                self.draw_staged();
                self.flush_visual()?;
            }
            match cmd {
                Command::SetState { clip, flags } => {
                    self.clip = Clip { x0: clip[0] as i32, y0: clip[1] as i32, x1: clip[2] as i32, y1: clip[3] as i32 };
                    self.skip = match flags & render::state::SKIP_MASK {
                        render::state::SKIP_EVEN => 0,
                        render::state::SKIP_ODD => 1,
                        _ => -1,
                    };
                }
                Command::Draw(recs) => {
                    // Runs of ordinary records draw as one batch; a serial
                    // record draws alone, row by row.
                    let mut run_start = 0;
                    for (i, rec) in recs.chunks_exact(PRIM_WORDS).enumerate() {
                        if rec[0] & render::prim::SERIAL != 0 {
                            self.draw_batch(&recs[run_start * PRIM_WORDS..i * PRIM_WORDS])?;
                            self.draw_serial(rec)?;
                            run_start = i + 1;
                        }
                    }
                    self.draw_batch(&recs[run_start * PRIM_WORDS..])?;
                }
                Command::Fill { rect, color } => {
                    let mut rec = vec![0u32; WORDS];
                    setup::fill(rect.x, rect.y, rect.w, rect.h, color, &mut rec);
                    self.draw_records(&mut rec, 1);
                }
                Command::Upload { rect, mask, pixels } => {
                    self.stats.uploads += 1;
                    self.stage(rect, mask, pixels);
                }
                Command::Copy { src_x, src_y, dst_x, dst_y, w, h, mask } => {
                    let mut rec = vec![0u32; WORDS];
                    setup::copy((src_x, src_y), (dst_x, dst_y), w, h, mask, &mut rec);
                    self.draw_records(&mut rec, 1);
                }
                Command::Readback(r) => {
                    let px = self.read_vram(r.x, r.y, r.w, r.h);
                    self.readbacks.extend_from_slice(&px);
                }
                Command::Present(desc) => self.present_frame(&desc)?,
                Command::Sync(rects) => {
                    // One copy of the rectangles' bounds: outside them the
                    // two copies already agree, and a blit call costs far
                    // more than the pixels it moves.
                    let mut b: Option<(u32, u32, u32, u32)> = None;
                    for pair in rects.chunks_exact(2) {
                        let r = render::Rect::from_words(pair[0], pair[1]);
                        for (x, y, w, h, _, _) in pieces(r.x, r.y, r.w, r.h) {
                            b = Some(b.map_or((x, y, x + w, y + h), |(x0, y0, x1, y1)| (x0.min(x), y0.min(y), x1.max(x + w), y1.max(y + h))));
                        }
                    }
                    if let Some((x0, y0, x1, y1)) = b {
                        self.sync(x0, y0, x1 - x0, y1 - y0);
                    }
                }
                Command::ClutLoad { slot, x, y, count } => unsafe {
                    self.stats.clut_loads += 1;
                    self.gl.disable(glow::SCISSOR_TEST);
                    self.gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(self.fbo_r));
                    self.gl.read_buffer(glow::COLOR_ATTACHMENT0);
                    self.gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(self.fbo_clut));
                    self.gl.draw_buffers(&[glow::COLOR_ATTACHMENT0]);
                    for (px, py, w, h, ox, _) in pieces(x, y, count, 1) {
                        let (dx, dy) = (ox as i32, slot as i32);
                        self.gl.blit_framebuffer(
                            px as i32,
                            py as i32,
                            (px + w) as i32,
                            (py + h) as i32,
                            dx,
                            dy,
                            dx + w as i32,
                            dy + 1,
                            glow::COLOR_BUFFER_BIT,
                            glow::NEAREST,
                        );
                    }
                },
            }
            if !matches!(cmd, Command::Draw(_) | Command::Present(_)) {
                self.visual_command(cmd)?;
            }
        }
        self.draw_staged();
        self.flush_visual()?;
        let err = unsafe { self.gl.get_error() };
        ensure!(err == glow::NO_ERROR, "GL error {err:#x}");
        Ok(())
    }

    /// The pixels READBACK commands produced, in order.
    pub fn take_readbacks(&mut self) -> Vec<u16> {
        std::mem::take(&mut self.readbacks)
    }

    /// Pack an upload's pixels into the staging texture's next free space,
    /// in shelves of rows, drawing what is staged first when it doesn't fit.
    fn stage(&mut self, rect: render::Rect, mask: u32, pixels: &[u32]) {
        if !self.staged.fits(rect) { self.draw_staged(); }
        self.staged.push(rect, mask, pixels);
    }

    /// Draw the staged uploads: one texture upload, one batch.
    fn draw_staged(&mut self) {
        if self.staged.count == 0 { return; }
        let mut staged = std::mem::take(&mut self.staged);
        staged.upload(&self.gl, self.stage);
        self.draw_records(&mut staged.records, staged.count);
        staged.clear();
        self.staged = staged;
    }

    /// Set up and draw primitive records as one batch.
    fn draw_batch(&mut self, recs: &[u32]) -> Result<()> {
        if recs.is_empty() { return Ok(()); }
        let n = recs.len() / PRIM_WORDS;
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.clear();
        scratch.resize(n * WORDS, 0);
        for (i, rec) in recs.chunks_exact(PRIM_WORDS).enumerate() {
            setup::primitive(rec, &self.clip, &mut scratch[i * WORDS..(i + 1) * WORDS]);
        }
        self.draw_records(&mut scratch, n);
        self.scratch = scratch;
        self.visual_command(Command::Draw(recs))
    }

    /// A primitive that samples where it draws, row by row in the GPU's
    /// raster order, each row's pixels synced to the read copy before the
    /// next row samples ([`render::prim::SERIAL`]).
    fn draw_serial(&mut self, rec: &[u32]) -> Result<()> {
        let mut one = [0u32; PRIM_WORDS];
        one.copy_from_slice(&rec[..PRIM_WORDS]);
        one[0] &= !render::prim::SERIAL;
        match one[0] & render::prim::KIND_MASK {
            render::prim::RECT => {
                let mut w = vec![0u32; WORDS];
                setup::primitive(&one, &self.clip, &mut w);
                let (ys, yb) = (w[14] as i32, w[15] as i32);
                if w[8] <= w[10] {
                    for y in ys..yb {
                        self.draw_row(&one, y)?;
                    }
                }
            }
            render::prim::TRIANGLE | render::prim::QUAD => {
                // A quad is its two triangles, one after the other.
                let quad = one[0] & render::prim::KIND_MASK == render::prim::QUAD;
                let mut tris = vec![one];
                if quad {
                    let mut second = one;
                    second[3..12].copy_from_slice(&one[6..15]);
                    tris[0][0] = (one[0] & !render::prim::KIND_MASK) | render::prim::TRIANGLE;
                    second[0] = tris[0][0];
                    tris.push(second);
                }
                for t in tris {
                    for y in setup::triangle_rows(&t) {
                        self.draw_row(&t, y)?;
                    }
                }
            }
            _ => self.draw_batch(&one)?,
        }
        Ok(())
    }

    /// Draw one row of a primitive (`y` as the drawing-area test sees it),
    /// then sync that row.
    fn draw_row(&mut self, rec: &[u32], y: i32) -> Result<()> {
        self.stats.serial_rows += 1;
        let saved = self.clip;
        if y < saved.y0 || y > saved.y1 { return Ok(()); }
        self.clip.y0 = y;
        self.clip.y1 = y;
        let result = self.draw_batch(rec);
        self.clip = saved;
        result?;
        if saved.x1 >= saved.x0 {
            self.sync(saved.x0 as u32, (y & 511) as u32, (saved.x1 - saved.x0 + 1) as u32, 1);
        }
        Ok(())
    }

    /// Copy draw-copy regions to the read copy.
    fn sync(&mut self, x: u32, y: u32, w: u32, h: u32) {
        self.stats.syncs += 1;
        unsafe {
            self.gl.disable(glow::SCISSOR_TEST);
            self.gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(self.fbo_w));
            self.gl.read_buffer(glow::COLOR_ATTACHMENT0);
            self.gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(self.fbo_r));
            self.gl.draw_buffers(&[glow::COLOR_ATTACHMENT0]);
            for (px, py, pw, ph, _, _) in pieces(x, y, w, h) {
                let (x0, y0, x1, y1) = (px as i32, py as i32, (px + pw) as i32, (py + ph) as i32);
                self.gl.blit_framebuffer(x0, y0, x1, y1, x0, y0, x1, y1, glow::COLOR_BUFFER_BIT, glow::NEAREST);
            }
        }
    }

    /// Draw `n` setup records (`words` is `n * WORDS` long; it is padded
    /// in place to whole texture rows).
    fn draw_records(&mut self, words: &mut Vec<u32>, n: usize) {
        self.raster.bind(&self.gl, self.canonical_vram(), self.stage, self.draw_state(), RasterTarget {
            framebuffer: self.fbo_w, scale: 1, destination: None,
        });
        let mut start = 0;
        while start < n {
            let m = PER_DRAW.min(n - start);
            self.raster.upload(&self.gl, words, start, m);
            self.raster.draw(&self.gl, 0, m);
            self.stats.draws += 1;
            start += m;
        }
    }

    /// What the backend did since the last call.
    pub fn take_stats(&mut self) -> Stats {
        self.stats.include(self.visual.take_stats());
        std::mem::take(&mut self.stats)
    }

    /// Read a VRAM rectangle (wrapping) from the draw copy, after
    /// everything executed so far: the `render_readback` host import.
    pub fn read_vram(&mut self, x: u32, y: u32, w: u32, h: u32) -> Vec<u16> {
        let mut out = vec![0u16; (w * h) as usize];
        let (format, ty, bpp) = self.read_format;
        raster::prepare_pack(&self.gl);
        unsafe {
            self.gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(self.fbo_w));
            self.gl.read_buffer(glow::COLOR_ATTACHMENT0);
            for (px, py, pw, ph, ox, oy) in pieces(x, y, w, h) {
                let mut buf = vec![0u8; (pw * ph) as usize * bpp];
                self.gl.read_pixels(
                    px as i32,
                    py as i32,
                    pw as i32,
                    ph as i32,
                    format,
                    ty,
                    glow::PixelPackData::Slice(Some(&mut buf)),
                );
                for row in 0..ph {
                    for col in 0..pw {
                        let i = (row * pw + col) as usize * bpp;
                        let v = u16::from_le_bytes([buf[i], buf[i + 1]]);
                        out[((oy + row) * w + ox + col) as usize] = v;
                    }
                }
            }
        }
        out
    }

    /// Render the display area into the RGBA8 output (row 0 at the top).
    fn present_frame(&mut self, desc: &PresentDesc) -> Result<()> {
        self.last_present = Some(*desc);
        let (vram, state) = (self.canonical_vram(), self.draw_state());
        self.last_frame = Some(self.visual.present(&VisualContext { gl: &self.gl, vram, state }, desc)?);
        Ok(())
    }

    /// The last presented frame, RGBA8 with row 0 at the top (frame
    /// dumps, tests).
    pub fn frame(&mut self) -> Option<Frame> {
        let VisualFrame { framebuffer: fbo, width: w, height: h } = self.last_frame?;
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        raster::prepare_pack(&self.gl);
        unsafe {
            self.gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(fbo));
            self.gl.read_buffer(glow::COLOR_ATTACHMENT0);
            self.gl.read_pixels(0, 0, w as i32, h as i32, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelPackData::Slice(Some(&mut rgba)));
        }
        // The output's row 0 is the picture's bottom row; flip to top-first.
        let stride = (w * 4) as usize;
        let mut top_first = Vec::with_capacity(rgba.len());
        for row in rgba.chunks_exact(stride).rev() {
            top_first.extend_from_slice(row);
        }
        Some(Frame { width: w, height: h, rgba: top_first })
    }

    /// Scale the last presented frame into the default framebuffer (a
    /// window's or a canvas's) of `w`×`h`; false before the first frame.
    pub fn blit_to_screen(&mut self, w: u32, h: u32) -> bool {
        let Some(VisualFrame { framebuffer: fbo, width: fw, height: fh }) = self.last_frame else { return false };
        unsafe {
            let gl = &self.gl;
            gl.disable(glow::SCISSOR_TEST);
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(fbo));
            gl.read_buffer(glow::COLOR_ATTACHMENT0);
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, None);
            gl.viewport(0, 0, w as i32, h as i32);
            gl.blit_framebuffer(0, 0, fw as i32, fh as i32, 0, 0, w as i32, h as i32, glow::COLOR_BUFFER_BIT, glow::NEAREST);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
        true
    }

    /// The last PRESENT's description.
    pub fn last_present(&self) -> Option<PresentDesc> {
        self.last_present
    }

    /// The GL context, for a host that draws the frame to a window.
    pub fn gl(&self) -> &glow::Context {
        &self.gl
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            let gl = &self.gl;
            self.visual.destroy(gl);
            self.raster.destroy(gl);
            for f in [self.fbo_w, self.fbo_r, self.fbo_clut] {
                gl.delete_framebuffer(f);
            }
            for t in [self.vram_w, self.vram_r, self.clut, self.stage] {
                gl.delete_texture(t);
            }
        }
    }
}

/// Errors of the command decoder as `anyhow`.
pub fn check_stream(words: &[u32]) -> Result<()> {
    for c in render::commands(words) {
        c.map_err(|e| anyhow::anyhow!("bad command stream: {e:?}")).context("checking")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pieces_split_at_the_edges() {
        let p: Vec<_> = pieces(1020, 510, 8, 4).collect();
        assert_eq!(p, vec![(1020, 510, 4, 2, 0, 0), (1020, 0, 4, 2, 0, 2), (0, 510, 4, 2, 4, 0), (0, 0, 4, 2, 4, 2)]);
        assert_eq!(pieces(0, 0, 1024, 512).count(), 1);
    }

    #[test]
    fn shaders_name_the_stride() {
        assert!(shaders::draw_fs().contains(&format!("* {} +", setup::STRIDE)));
    }
}
