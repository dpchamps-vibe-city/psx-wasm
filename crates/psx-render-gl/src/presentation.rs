//! Shared native/visual-integer-colour-plane presentation. Other visual renderers may
//! provide their own presentation and return any RGBA8 VisualFrame instead.

use anyhow::{Context as _, Result};
use glow::HasContext;
use psxw_abi::render::PresentDesc;

use crate::{framebuffer, gl_err, program, shaders, texture, visual::VisualFrame};

pub(crate) struct Presenter {
    program: glow::Program,
    vao: glow::VertexArray,
    area: Option<glow::UniformLocation>,
    depth24: Option<glow::UniformLocation>,
    high_precision: Option<glow::UniformLocation>,
    scale: Option<glow::UniformLocation>,
    out: Option<(glow::Texture, VisualFrame)>,
}

impl Presenter {
    pub fn new(gl: &glow::Context) -> Result<Self> {
        unsafe {
            let p = program(gl, &shaders::present_vs(), &shaders::present_fs())?;
            let vao = match gl.create_vertex_array() {
                Ok(vao) => vao,
                Err(e) => {
                    gl.delete_program(p);
                    return Err(gl_err(e));
                }
            };
            gl.use_program(Some(p));
            gl.uniform_1_i32(gl.get_uniform_location(p, "u_vram").as_ref(), 0);
            gl.uniform_1_i32(gl.get_uniform_location(p, "u_flip").as_ref(), 1);
            Ok(Self {
                program: p,
                vao,
                area: gl.get_uniform_location(p, "u_area"),
                depth24: gl.get_uniform_location(p, "u_depth24"),
                high_precision: gl.get_uniform_location(p, "u_high_precision"),
                scale: gl.get_uniform_location(p, "u_scale"),
                out: None,
            })
        }
    }

    pub fn present(
        &mut self,
        gl: &glow::Context,
        source: glow::Texture,
        scale: u32,
        desc: &PresentDesc,
    ) -> Result<VisualFrame> {
        self.present_with_precision(gl, source, scale, desc, false)
    }

    pub fn present_with_precision(
        &mut self,
        gl: &glow::Context,
        source: glow::Texture,
        scale: u32,
        desc: &PresentDesc,
        high_precision: bool,
    ) -> Result<VisualFrame> {
        crate::raster::prepare_draw(gl);
        let (nw, nh) = (desc.width.clamp(1, 1024), desc.height.clamp(1, 512));
        let (w, h) = (nw * scale, nh * scale);
        unsafe {
            if !matches!(self.out, Some((_, f)) if f.width == w && f.height == h) {
                let tex = texture(gl, glow::RGBA8, w as i32, h as i32).context("present: output texture")?;
                let fbo = match framebuffer(gl, tex) {
                    Ok(f) => f,
                    Err(e) => {
                        gl.delete_texture(tex);
                        return Err(e.context("present: output framebuffer"));
                    }
                };
                self.reset(gl);
                self.out = Some((tex, VisualFrame { framebuffer: fbo, width: w, height: h }));
            }
            let frame = self.out.as_ref().expect("allocated output").1;
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(frame.framebuffer));
            gl.draw_buffers(&[glow::COLOR_ATTACHMENT0]);
            gl.viewport(0, 0, w as i32, h as i32);
            if desc.flags & PresentDesc::ENABLED == 0 {
                gl.clear_color(0.0, 0.0, 0.0, 1.0);
                gl.clear(glow::COLOR_BUFFER_BIT);
            } else {
                gl.use_program(Some(self.program));
                gl.bind_vertex_array(Some(self.vao));
                gl.active_texture(glow::TEXTURE0);
                gl.bind_sampler(0, None);
                gl.bind_texture(glow::TEXTURE_2D, Some(source));
                gl.uniform_4_i32(self.area.as_ref(), desc.x as i32, desc.y as i32, nw as i32, nh as i32);
                gl.uniform_1_i32(self.depth24.as_ref(), (desc.flags & PresentDesc::DEPTH24 != 0) as i32);
                gl.uniform_1_i32(self.high_precision.as_ref(), high_precision as i32);
                gl.uniform_1_i32(self.scale.as_ref(), scale as i32);
                gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            }
            Ok(frame)
        }
    }

    pub fn reset(&mut self, gl: &glow::Context) {
        if let Some((t, f)) = self.out.take() {
            unsafe {
                gl.delete_framebuffer(f.framebuffer);
                gl.delete_texture(t);
            }
        }
    }

    pub fn destroy(&mut self, gl: &glow::Context) {
        self.reset(gl);
        unsafe {
            gl.delete_program(self.program);
            gl.delete_vertex_array(self.vao);
        }
    }
}
