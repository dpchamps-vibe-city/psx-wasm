#![cfg(feature = "headless")]

use std::{cell::RefCell, rc::Rc};

use anyhow::Result;
use glow::HasContext;
use psx_render_gl::{
    Renderer,
    headless::Headless,
    setup::Clip,
    visual::{VisualContext, VisualFrame, VisualRenderer},
};
use psxw_abi::render::{Command, PresentDesc, header, op, pack_xy, prim};

#[derive(Default)]
struct Trace {
    commands: Vec<&'static str>,
    draws: Vec<(usize, Clip)>,
    flushes: usize,
    resets: usize,
    destroyed: usize,
}

// Deliberately not the built-in rasterizer: independent RGBA output, arbitrary
// dimensions, no setup-record format, and different pipeline state.
struct Magenta {
    texture: glow::Texture,
    framebuffer: glow::Framebuffer,
    canonical: glow::Texture,
    trace: Rc<RefCell<Trace>>,
}

impl Magenta {
    fn new(ctx: &VisualContext<'_>, trace: Rc<RefCell<Trace>>) -> Result<Self> {
        unsafe {
            let gl = ctx.gl;
            let texture = gl.create_texture().map_err(anyhow::Error::msg)?;
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.tex_storage_2d(glow::TEXTURE_2D, 1, glow::RGBA8, 7, 5);
            let framebuffer = gl.create_framebuffer().map_err(anyhow::Error::msg)?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(texture), 0);
            assert_eq!(gl.check_framebuffer_status(glow::FRAMEBUFFER), glow::FRAMEBUFFER_COMPLETE);
            Ok(Self { texture, framebuffer, canonical: ctx.vram.draw, trace })
        }
    }
}

impl VisualRenderer for Magenta {
    fn execute(&mut self, ctx: &VisualContext<'_>, cmd: Command<'_>) -> Result<()> {
        let mut trace = self.trace.borrow_mut();
        let name = match cmd {
            Command::SetState { .. } => "state",
            Command::Draw(recs) => {
                trace.draws.push((recs.len(), ctx.state.clip));
                "draw"
            }
            Command::Fill { .. } => "fill",
            Command::Upload { pixels, .. } => {
                assert_eq!(pixels, &[0x7c00]);
                "upload"
            }
            Command::Copy { .. } => "copy",
            Command::Readback(_) => "readback",
            Command::Sync(_) => "sync",
            Command::ClutLoad { .. } => "clut",
            Command::Present(_) => panic!("presentation has its own callback"),
        };
        trace.commands.push(name);
        // These ordinary GL choices must not leak into exact drawing, blits or
        // CPU transfers. We never modify a canonical texture or attachment.
        unsafe {
            ctx.gl.enable(glow::SCISSOR_TEST);
            ctx.gl.scissor(0, 0, 0, 0);
            ctx.gl.enable(glow::CULL_FACE);
            ctx.gl.enable(glow::RASTERIZER_DISCARD);
            ctx.gl.color_mask(false, false, false, false);
            ctx.gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 64);
            ctx.gl.pixel_store_i32(glow::UNPACK_SKIP_PIXELS, 1);
            ctx.gl.pixel_store_i32(glow::PACK_ROW_LENGTH, 64);
            ctx.gl.pixel_store_i32(glow::PACK_SKIP_PIXELS, 1);
        }
        Ok(())
    }

    fn flush(&mut self, _ctx: &VisualContext<'_>) -> Result<()> {
        self.trace.borrow_mut().flushes += 1;
        Ok(())
    }

    fn reset(&mut self, _ctx: &VisualContext<'_>) {
        self.trace.borrow_mut().resets += 1;
    }

    fn present(&mut self, ctx: &VisualContext<'_>, _desc: &PresentDesc) -> Result<VisualFrame> {
        unsafe {
            let gl = ctx.gl;
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::RASTERIZER_DISCARD);
            gl.color_mask(true, true, true, true);
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(self.framebuffer));
            gl.clear_color(1.0, 0.0, 1.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
        }
        Ok(VisualFrame { framebuffer: self.framebuffer, width: 7, height: 5 })
    }

    fn destroy(&mut self, gl: &glow::Context) {
        unsafe {
            assert!(gl.is_texture(self.canonical), "visual renderer is destroyed before canonical VRAM");
            gl.delete_framebuffer(self.framebuffer);
            gl.delete_texture(self.texture);
        }
        self.trace.borrow_mut().destroyed += 1;
    }
}

fn command(out: &mut Vec<u32>, opcode: u8, payload: &[u32]) {
    out.push(header(opcode, payload.len() as u32));
    out.extend_from_slice(payload);
}

#[test]
fn external_visual_renderer_gets_commands_and_owns_output_but_never_game_readbacks() {
    let (_egl, gl) = Headless::new().unwrap();
    let mut r = Renderer::new(gl).unwrap();
    let mut stream = Vec::new();
    command(&mut stream, op::SET_STATE, &[0, pack_xy(31, 31), 0]);
    command(&mut stream, op::FILL, &[0, 32 | 32 << 16, 0x001f]);
    command(&mut stream, op::SYNC, &[1, 0, 32 | 32 << 16]);
    let mut rect = [0; 16];
    rect[0] = prim::RECT | prim::TEXTURED | prim::RAW | prim::SERIAL | 2 << prim::DEPTH_SHIFT;
    rect[3] = pack_xy(4, 4);
    rect[4] = 4 | 3 << 16;
    command(&mut stream, op::DRAW, &[vec![1], rect.to_vec()].concat());
    command(&mut stream, op::UPLOAD, &[pack_xy(31, 31), 1 | 1 << 16, 0, 0x7c00]);
    command(&mut stream, op::SYNC, &[1, pack_xy(31, 31), 1 | 1 << 16]);
    command(&mut stream, op::COPY, &[pack_xy(31, 31), pack_xy(30, 30), 1 | 1 << 16, 0]);
    command(&mut stream, op::CLUT_LOAD, &[0, 0, 16]);
    command(&mut stream, op::READBACK, &[0, 32 | 32 << 16]);
    r.execute(&stream).unwrap();
    let expected = r.take_readbacks();
    r.reset_vram();

    let trace = Rc::new(RefCell::new(Trace::default()));
    r.set_visual_renderer(|ctx| Ok(Box::new(Magenta::new(ctx, trace.clone())?))).unwrap();
    r.execute(&stream).unwrap();
    assert_eq!(r.take_readbacks(), expected);
    assert_eq!(r.read_vram(0, 0, 32, 32), expected);
    assert_eq!(
        trace.borrow().draws.iter().map(|(n, c)| (*n, c.y0, c.y1)).collect::<Vec<_>>(),
        [(16, 4, 4), (16, 5, 5), (16, 6, 6)]
    );
    assert_eq!(
        trace.borrow().commands,
        ["state", "fill", "sync", "draw", "draw", "draw", "upload", "sync", "copy", "clut", "readback"]
    );
    assert!(trace.borrow().flushes > 0);

    let mut present = Vec::new();
    command(
        &mut present,
        op::PRESENT,
        &PresentDesc { flags: PresentDesc::ENABLED, x: 0, y: 0, width: 32, height: 32 }.to_words(),
    );
    r.execute(&present).unwrap();
    let f = r.frame().unwrap();
    assert_eq!((f.width, f.height), (7, 5));
    assert!(f.rgba.chunks_exact(4).all(|p| p == [255, 0, 255, 255]));
    assert!(r.set_visual_renderer(|_| anyhow::bail!("factory failed")).is_err());
    assert_eq!(r.output_size(), Some((7, 5)));
    assert_eq!(trace.borrow().destroyed, 0);
    r.reset_vram();
    assert_eq!(trace.borrow().resets, 1);
    assert_eq!(r.output_size(), None);
    // Native selection must replace a custom renderer even though both report 1×.
    r.set_resolution_scale(1).unwrap();
    assert_eq!(trace.borrow().destroyed, 1);
    r.execute(&present).unwrap();
    assert_eq!(r.output_size(), Some((32, 32)));
    assert!(r.frame().unwrap().rgba.chunks_exact(4).all(|p| p == [0, 0, 0, 255]));
}
