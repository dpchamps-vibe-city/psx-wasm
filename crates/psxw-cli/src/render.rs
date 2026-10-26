//! A renderer for native runs: the render server in its pinned image (M4),
//! the same backend the GPU oracle checks, behind the host's `Render`.

use anyhow::Result;
use psxw_abi::render::{PresentDesc, Rect, op};
use psxw_gpu_oracle::GpuDevice;
use psxw_host_native::render::{Render, SharedRender};
use psxw_inputs::Workspace;

pub struct ServerRender {
    dev: GpuDevice,
}

fn header(op: u8, words: u32) -> u32 {
    op as u32 | words << 8
}

impl Render for ServerRender {
    fn submit(&mut self, words: &[u32]) -> Result<()> {
        self.dev.execute(words).map(|_| ())
    }
    fn readback(&mut self, r: Rect) -> Result<Vec<u16>> {
        let mut w = vec![header(op::READBACK, 2)];
        w.extend_from_slice(&r.words());
        self.dev.execute(&w)
    }
    fn present(&mut self, desc: &PresentDesc) -> Result<()> {
        let mut w = vec![header(op::PRESENT, psxw_abi::render::PRESENT_WORDS as u32)];
        w.extend_from_slice(&desc.to_words());
        self.dev.execute(&w).map(|_| ())
    }
    fn vram(&mut self) -> Result<Option<Vec<u16>>> {
        self.readback(Rect { x: 0, y: 0, w: 1024, h: 512 }).map(Some)
    }
    fn frame(&mut self) -> Result<Option<(u32, u32, Vec<u8>)>> {
        self.dev.frame().map(Some)
    }
}

pub fn server(ws: &Workspace) -> SharedRender {
    let mut dev = GpuDevice::new(ws);
    let _ = dev.reset();
    std::sync::Arc::new(std::sync::Mutex::new(Box::new(ServerRender { dev })))
}
