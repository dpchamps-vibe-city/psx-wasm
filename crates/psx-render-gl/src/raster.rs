//! GL helpers shared by the exact rasterizer and the built-in upscaler.
//! These are optional implementation details, not the VisualRenderer contract.

use anyhow::Result;
use glow::HasContext;
use psxw_abi::render::Rect;

use crate::{
    gl_err, program,
    setup::{self, STRIDE, WORDS},
    shaders::{self, PRIM_TEX_WIDTH},
    texture,
    visual::{CanonicalVram, DrawState},
};

// Visual implementations share the context but not pipeline state. Establish
// the fixed state required by exact integer drawing after any custom callback.
pub(crate) fn prepare_draw(gl: &glow::Context) {
    unsafe {
        for state in [
            glow::BLEND,
            glow::DEPTH_TEST,
            glow::SCISSOR_TEST,
            glow::CULL_FACE,
            glow::STENCIL_TEST,
            glow::DITHER,
            glow::RASTERIZER_DISCARD,
        ] {
            gl.disable(state);
        }
        gl.color_mask(true, true, true, true);
    }
}

pub(crate) fn prepare_unpack(gl: &glow::Context) {
    unsafe {
        gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
        for (name, value) in [
            (glow::UNPACK_ALIGNMENT, 1),
            (glow::UNPACK_ROW_LENGTH, 0),
            (glow::UNPACK_SKIP_ROWS, 0),
            (glow::UNPACK_SKIP_PIXELS, 0),
        ] {
            gl.pixel_store_i32(name, value);
        }
    }
}

pub(crate) fn prepare_pack(gl: &glow::Context) {
    unsafe {
        gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
        for (name, value) in [
            (glow::PACK_ALIGNMENT, 1),
            (glow::PACK_ROW_LENGTH, 0),
            (glow::PACK_SKIP_ROWS, 0),
            (glow::PACK_SKIP_PIXELS, 0),
        ] {
            gl.pixel_store_i32(name, value);
        }
    }
}

const PRIM_TEX_ROWS: usize = 64;
pub(crate) const PER_DRAW: usize = PRIM_TEX_WIDTH * PRIM_TEX_ROWS / STRIDE;

pub(crate) struct RasterTarget {
    pub framebuffer: glow::Framebuffer,
    pub scale: i32,
    pub destination: Option<glow::Texture>,
}

pub(crate) struct RasterPass {
    program: glow::Program,
    prims: glow::Texture,
    vao: glow::VertexArray,
    first: Option<glow::UniformLocation>,
    clip: Option<glow::UniformLocation>,
    skip: Option<glow::UniformLocation>,
    scale: Option<glow::UniformLocation>,
}

impl RasterPass {
    pub fn new(gl: &glow::Context, fragment: &str) -> Result<Self> {
        unsafe {
            let p = program(gl, &shaders::draw_vs(), fragment)?;
            let prims = match texture(gl, glow::RGBA32UI, PRIM_TEX_WIDTH as i32, PRIM_TEX_ROWS as i32) {
                Ok(t) => t,
                Err(e) => {
                    gl.delete_program(p);
                    return Err(e);
                }
            };
            let vao = match gl.create_vertex_array() {
                Ok(v) => v,
                Err(e) => {
                    gl.delete_texture(prims);
                    gl.delete_program(p);
                    return Err(gl_err(e));
                }
            };
            gl.use_program(Some(p));
            for (name, unit) in [("u_vram", 0), ("u_prims", 1), ("u_clut", 2), ("u_stage", 3), ("u_dest", 4), ("u_super", 5)] {
                gl.uniform_1_i32(gl.get_uniform_location(p, name).as_ref(), unit);
            }
            Ok(Self {
                program: p,
                prims,
                vao,
                first: gl.get_uniform_location(p, "u_first"),
                clip: gl.get_uniform_location(p, "u_clip"),
                skip: gl.get_uniform_location(p, "u_skip"),
                scale: gl.get_uniform_location(p, "u_scale"),
            })
        }
    }

    pub fn bind(
        &self,
        gl: &glow::Context,
        vram: CanonicalVram,
        stage: glow::Texture,
        state: DrawState,
        target: RasterTarget,
    ) {
        prepare_draw(gl);
        unsafe {
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(target.framebuffer));
            gl.draw_buffers(&[glow::COLOR_ATTACHMENT0]);
            gl.viewport(0, 0, 1024 * target.scale, 512 * target.scale);
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vao));
            let c = state.clip;
            gl.uniform_4_i32(self.clip.as_ref(), c.x0, c.y0, c.x1, c.y1);
            gl.uniform_1_i32(self.skip.as_ref(), state.skip);
            gl.uniform_1_i32(self.scale.as_ref(), target.scale);
            if let Some(t) = target.destination {
                gl.active_texture(glow::TEXTURE4);
                gl.bind_sampler(4, None);
                gl.bind_texture(glow::TEXTURE_2D, Some(t));
            }
            for (unit, tex) in [(0, vram.read), (1, self.prims), (2, vram.clut), (3, stage)] {
                gl.active_texture(glow::TEXTURE0 + unit);
                gl.bind_sampler(unit, None);
                gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            }
        }
    }

    /// Upload one chunk of setup records and pad the final chunk to texture rows.
    pub fn upload(&self, gl: &glow::Context, words: &mut Vec<u32>, start: usize, count: usize) {
        prepare_unpack(gl);
        let rows = (count * STRIDE).div_ceil(PRIM_TEX_WIDTH);
        let chunk_words = rows * PRIM_TEX_WIDTH * 4;
        let from = start * WORDS;
        if words.len() < from + chunk_words {
            words.resize(from + chunk_words, 0);
        }
        unsafe {
            let bytes = std::slice::from_raw_parts(words[from..].as_ptr() as *const u8, chunk_words * 4);
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.prims));
            gl.tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                0,
                0,
                PRIM_TEX_WIDTH as i32,
                rows as i32,
                glow::RGBA_INTEGER,
                glow::UNSIGNED_INT,
                glow::PixelUnpackData::Slice(Some(bytes)),
            );
        }
    }

    pub fn draw(&self, gl: &glow::Context, first: usize, count: usize) {
        if count == 0 {
            return;
        }
        unsafe {
            gl.uniform_1_i32(self.first.as_ref(), first as i32);
            gl.draw_arrays_instanced(glow::TRIANGLE_STRIP, 0, 4, count as i32);
        }
    }

    pub fn destroy(&mut self, gl: &glow::Context) {
        unsafe {
            gl.delete_program(self.program);
            gl.delete_texture(self.prims);
            gl.delete_vertex_array(self.vao);
        }
    }
}

/// Shelf-packed uploads, independently buffered by each VRAM implementation.
#[derive(Default)]
pub(crate) struct UploadBatch {
    px: Vec<u16>,
    x: u32,
    y: u32,
    row_h: u32,
    w: u32,
    pub records: Vec<u32>,
    pub count: usize,
}

impl UploadBatch {
    pub fn fits(&self, rect: Rect) -> bool {
        let y = if self.x + rect.w > 1024 { self.y + self.row_h } else { self.y };
        y + rect.h <= 512
    }

    pub fn push(&mut self, rect: Rect, mask: u32, pixels: &[u32]) {
        if self.x + rect.w > 1024 {
            (self.x, self.y, self.row_h) = (0, self.y + self.row_h, 0);
        }
        self.px.resize(1024 * 512, 0);
        let (sx, sy) = (self.x, self.y);
        for j in 0..rect.h {
            let row = ((sy + j) * 1024 + sx) as usize;
            for i in 0..rect.w as usize {
                let k = (j * rect.w) as usize + i;
                self.px[row + i] = (pixels[k / 2] >> (16 * (k % 2))) as u16;
            }
        }
        self.x += rect.w;
        self.row_h = self.row_h.max(rect.h);
        self.w = self.w.max(self.x);
        let at = self.records.len();
        self.records.resize(at + WORDS, 0);
        setup::upload(rect.x, rect.y, rect.w, rect.h, sx, sy, mask, &mut self.records[at..]);
        self.count += 1;
    }

    pub fn upload(&self, gl: &glow::Context, stage: glow::Texture) {
        prepare_unpack(gl);
        let rows = self.y + self.row_h;
        unsafe {
            let bytes = std::slice::from_raw_parts(self.px.as_ptr() as *const u8, rows as usize * 1024 * 2);
            gl.active_texture(glow::TEXTURE3);
            gl.bind_texture(glow::TEXTURE_2D, Some(stage));
            gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 1024);
            gl.tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                0,
                0,
                self.w as i32,
                rows as i32,
                glow::RED_INTEGER,
                glow::UNSIGNED_SHORT,
                glow::PixelUnpackData::Slice(Some(bytes)),
            );
            gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0);
        }
    }

    pub fn clear(&mut self) {
        self.records.clear();
        (self.x, self.y, self.row_h, self.w, self.count) = (0, 0, 0, 0, 0);
    }
}
