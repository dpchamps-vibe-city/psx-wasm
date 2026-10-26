//! The host's renderer behind the `render_submit`, `render_readback` and
//! `present` imports (`psxw_abi::render`). Headless runs default to
//! [`NullRender`], which executes nothing: readbacks are black. A real
//! backend (the render server in its pinned image, M4) plugs in through
//! [`Render`].

use anyhow::Result;
use psxw_abi::render::{PresentDesc, Rect};
use serde::Serialize;

pub trait Render: Send {
    /// Execute renderer commands.
    fn submit(&mut self, words: &[u32]) -> Result<()>;
    /// A VRAM rectangle after everything submitted, `w * h` pixels.
    fn readback(&mut self, r: Rect) -> Result<Vec<u16>>;
    /// Show a frame.
    fn present(&mut self, desc: &PresentDesc) -> Result<()>;
    /// The whole of VRAM (1024 × 512), when the backend has one.
    fn vram(&mut self) -> Result<Option<Vec<u16>>> {
        Ok(None)
    }
}

/// Executes nothing: readbacks are zeros.
#[derive(Debug, Default)]
pub struct NullRender;

impl Render for NullRender {
    fn submit(&mut self, _words: &[u32]) -> Result<()> {
        Ok(())
    }
    fn readback(&mut self, r: Rect) -> Result<Vec<u16>> {
        Ok(vec![0; (r.w * r.h) as usize])
    }
    fn present(&mut self, _desc: &PresentDesc) -> Result<()> {
        Ok(())
    }
}

/// What the run sent the renderer.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RenderStats {
    pub submits: u64,
    pub words: u64,
    pub readbacks: u64,
    pub presents: u64,
    /// The last display the game presented.
    pub last_present: Option<[u32; 5]>,
}

pub type SharedRender = std::sync::Arc<std::sync::Mutex<Box<dyn Render>>>;

impl std::fmt::Debug for dyn Render {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("dyn Render")
    }
}
