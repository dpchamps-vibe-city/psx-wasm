//! Host-local visual rendering, independent of authoritative VRAM execution.
//!
//! Implement [`VisualRenderer`] and install it with
//! [`crate::Renderer::set_visual_renderer`]. Implementations receive decoded ABI
//! commands, not GL setup records. They own their resources, rasterization,
//! hazards and output. They must not modify canonical VRAM or answer game reads.
//! This is a WebGL2/GLES extension point, not a cross-graphics-API abstraction.

use anyhow::Result;
use glow::HasContext;
use psxw_abi::render::{Command, PresentDesc};

use crate::{Stats, enhanced::Enhanced, presentation::Presenter, setup::Clip};

/// Current effective drawing state. Feedback primitives are delivered as
/// clipped row draws, so use this state rather than caching SET_STATE yourself.
#[derive(Debug, Clone, Copy)]
pub struct DrawState {
    pub clip: Clip,
    /// Native row parity to skip, or -1.
    pub skip: i32,
}

/// Read-only by contract: these textures belong to the exact VRAM engine.
/// `draw` includes all preceding writes; only SYNC updates `read`. CLUT slots
/// retain the GPU's cache semantics. Never attach these textures for writing.
#[derive(Clone, Copy)]
pub struct CanonicalVram {
    pub draw: glow::Texture,
    pub read: glow::Texture,
    pub clut: glow::Texture,
    pub(crate) framebuffer: glow::Framebuffer,
}

impl CanonicalVram {
    /// Initialize a visual colour plane without a CPU readback. The destination
    /// must be an R16UI framebuffer of 1024*scale by 512*scale pixels.
    pub fn seed(&self, gl: &glow::Context, destination: glow::Framebuffer, scale: i32) {
        unsafe {
            gl.disable(glow::SCISSOR_TEST);
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(self.framebuffer));
            gl.read_buffer(glow::COLOR_ATTACHMENT0);
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(destination));
            gl.draw_buffers(&[glow::COLOR_ATTACHMENT0]);
            gl.blit_framebuffer(
                0,
                0,
                1024,
                512,
                0,
                0,
                1024 * scale,
                512 * scale,
                glow::COLOR_BUFFER_BIT,
                glow::NEAREST,
            );
        }
    }
}

/// Borrowed services and canonical inputs, never the concrete parent renderer.
pub struct VisualContext<'a> {
    pub gl: &'a glow::Context,
    pub vram: CanonicalVram,
    pub state: DrawState,
}

/// Output owned by a visual renderer, with RGBA8 at COLOR_ATTACHMENT0 and
/// GL row 0 at the bottom.
/// Remains valid until its next presentation, reset, or destruction.
#[derive(Clone, Copy)]
pub struct VisualFrame {
    pub framebuffer: glow::Framebuffer,
    pub width: u32,
    pub height: u32,
}

/// Callbacks share the GL context: establish the state your own rendering needs.
/// The exact engine reestablishes its draw/transfer state after callbacks.
pub trait VisualRenderer {
    /// Commands arrive in order after their exact execution. DRAW uses the
    /// original ABI primitive records; feedback draws are expanded row by row
    /// before the next native SYNC. PRESENT is delivered via `present` instead.
    fn execute(&mut self, _ctx: &VisualContext<'_>, _command: Command<'_>) -> Result<()> {
        Ok(())
    }
    /// Finish buffered work before canonical sources change or execution returns.
    fn flush(&mut self, _ctx: &VisualContext<'_>) -> Result<()> {
        Ok(())
    }
    fn reset(&mut self, ctx: &VisualContext<'_>);
    fn present(&mut self, ctx: &VisualContext<'_>, desc: &PresentDesc) -> Result<VisualFrame>;
    fn take_stats(&mut self) -> Stats {
        Stats::default()
    }
    /// Release GL resources while the original context is still alive/current.
    fn destroy(&mut self, gl: &glow::Context);
}

/// Host-only factory for the built-in presets. Custom renderers need no entry
/// here: install their factory directly with Renderer::set_visual_renderer.
pub(crate) fn create(ctx: &VisualContext<'_>, scale: u32) -> Result<Box<dyn VisualRenderer>> {
    match scale {
        1 => Ok(Box::new(Native { presenter: Presenter::new(ctx.gl)? })),
        _ => Ok(Box::new(Enhanced::new(ctx, scale as i32)?)),
    }
}

struct Native {
    presenter: Presenter,
}

impl VisualRenderer for Native {
    fn reset(&mut self, ctx: &VisualContext<'_>) {
        self.presenter.reset(ctx.gl);
    }
    fn present(&mut self, ctx: &VisualContext<'_>, desc: &PresentDesc) -> Result<VisualFrame> {
        self.presenter.present(ctx.gl, ctx.vram.draw, 1, desc)
    }
    fn destroy(&mut self, gl: &glow::Context) {
        self.presenter.destroy(gl);
    }
}
